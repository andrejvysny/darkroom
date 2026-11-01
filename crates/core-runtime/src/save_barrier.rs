//! Save-barrier coordination shared by a normal quit and by an updater relaunch (Tauri-free).
//!
//! Develop edits are debounced in the webview before they reach the catalog, so at the moment the
//! user presses quit (or accepts an update) the newest edit can still be sitting in a JavaScript
//! timer. The exit is held, the frontend is asked to flush, and the caller proceeds only once the
//! frontend acknowledged a successful save *and* the catalog confirms it.
//!
//! Rules:
//! - Every barrier has an explicit [`BarrierId`]. The flush request carries it and the frontend
//!   echoes it back; an ack for any other id (late, stray, from an older barrier) is ignored.
//! - An ack is `ok` plus the revision of every edit the frontend claims to have saved. The
//!   adapter-supplied `verify` closure checks the claim against the catalog; a claim the catalog
//!   cannot back is [`BarrierResult::Failed`], never `Flushed`.
//! - No ack within the timeout is [`BarrierResult::NoAcknowledgement`] and is never "saved".
//! - The coordinator ([`SaveBarrier::run`]) must run OFF the ordered `UserCommand` lane: the
//!   frontend's flush is itself an ordered `develop_set_edit` job, so a barrier occupying that lane
//!   could not receive the save it is waiting for.
//!
//! The escape hatch is deliberate: the *next* request of the same reason within [`RETRY_GRACE`]
//! after a barrier that did not reach disk proceeds unconditionally. The user was told the save
//! failed and asked again, which is an explicit decision to discard the in-memory edit. Grace is
//! keyed per [`BarrierReason`].

use core_db::rusqlite::{named_params, Connection, OptionalExtension};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long to wait for the frontend's ack. The frontend's flush is a real ordered save job
/// (catalog lock, transaction, event append) that may queue behind other ordered user commands, so
/// it must fit inside this window; 1.5 s was too tight for that.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// After a barrier that did not reach disk, a second request inside this window is honoured
/// unconditionally — see the module docs.
pub const RETRY_GRACE: Duration = Duration::from_secs(30);

/// Monotonic barrier identity; never reused within a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BarrierId(pub u64);

/// An edit revision the frontend claims is committed for `image_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavedRevision {
    pub image_id: i64,
    pub revision: i64,
}

/// The frontend's answer to a flush request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AckVerdict {
    pub ok: bool,
    pub saved: Vec<SavedRevision>,
}

impl AckVerdict {
    pub fn ok(saved: Vec<SavedRevision>) -> Self {
        Self { ok: true, saved }
    }
    pub fn failed() -> Self {
        Self {
            ok: false,
            saved: Vec::new(),
        }
    }
}

/// Who asked for the barrier. Reported to the UI so it can word the prompt for the right action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierReason {
    /// A user-initiated quit (`⌘Q`).
    Quit,
    /// The updater is about to relaunch the process.
    UpdaterRestart,
}

impl BarrierReason {
    pub fn label(self) -> &'static str {
        match self {
            BarrierReason::Quit => "quit",
            BarrierReason::UpdaterRestart => "updater-restart",
        }
    }

    /// What the user was doing, for the log.
    pub fn verb(self) -> &'static str {
        match self {
            BarrierReason::Quit => "quit",
            BarrierReason::UpdaterRestart => "restart",
        }
    }
}

/// How the barrier ended. Only `Flushed` authorizes leaving the process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierResult {
    /// The frontend confirmed every debounced edit is committed and the catalog agrees.
    Flushed,
    /// A save failed, the claim could not be verified, or the barrier was cancelled.
    Failed(String),
    /// The frontend never answered. Treated exactly like a failure: nothing is discarded silently.
    NoAcknowledgement,
}

impl BarrierResult {
    /// Whether the caller may now exit / relaunch.
    pub fn is_safe(&self) -> bool {
        matches!(self, BarrierResult::Flushed)
    }

    /// The user-facing explanation, shared by the log, the event payload and the updater banner.
    pub fn message(&self, reason: BarrierReason) -> String {
        match self {
            BarrierResult::Flushed => String::new(),
            BarrierResult::Failed(error) => error.clone(),
            BarrierResult::NoAcknowledgement => format!(
                "The editor did not acknowledge the save within {} ms, so this {} was cancelled \
                 and nothing was discarded. Save manually if you can, or {} again within {} s to \
                 discard unsaved Develop edits.",
                ACK_TIMEOUT.as_millis(),
                reason.verb(),
                reason.verb(),
                RETRY_GRACE.as_secs(),
            ),
        }
    }
}

enum Message {
    Ack(AckVerdict),
    Cancelled,
}

/// Handed to the winner of [`SaveBarrier::begin`]; consumed by [`SaveBarrier::run`].
pub struct Ticket {
    pub id: BarrierId,
    rx: Receiver<Message>,
}

/// What claiming the barrier decided.
pub enum Begin {
    /// Let the caller exit now (the grace window: the user already chose to discard).
    Proceed,
    /// A barrier is already running; this request must not escape on its own.
    Hold,
    /// Run the barrier with this ticket.
    Start(Ticket),
}

#[derive(Default)]
struct State {
    next_id: u64,
    /// The barrier currently waiting, with where its ack is delivered.
    running: Option<(BarrierId, SyncSender<Message>)>,
    /// A barrier has been claimed (possibly not yet waiting) and not yet settled.
    claimed: bool,
    last_failure: Option<(BarrierReason, Instant)>,
}

/// One barrier at a time, plus the grace window that keeps a broken disk from trapping the app.
#[derive(Default)]
pub struct SaveBarrier {
    state: Mutex<State>,
}

impl SaveBarrier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scheduling state only — recover from poisoning rather than killing the save path.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Claim the barrier for this request.
    pub fn begin(&self, reason: BarrierReason) -> Begin {
        let mut st = self.lock();
        if st.claimed {
            return Begin::Hold;
        }
        if st
            .last_failure
            .is_some_and(|(failed, t)| failed == reason && t.elapsed() < RETRY_GRACE)
        {
            st.last_failure = None;
            return Begin::Proceed;
        }
        st.next_id += 1;
        let id = BarrierId(st.next_id);
        let (tx, rx) = sync_channel(1);
        st.claimed = true;
        st.running = Some((id, tx));
        Begin::Start(Ticket { id, rx })
    }

    fn take_running(&self, id: BarrierId) -> Option<SyncSender<Message>> {
        let mut st = self.lock();
        match st.running {
            Some((running, _)) if running == id => st.running.take().map(|(_, tx)| tx),
            _ => None,
        }
    }

    /// Deliver the frontend's answer. Returns whether it was accepted: an ack whose id is not the
    /// running barrier's (late, stray, already answered) is ignored and leaves that barrier waiting.
    pub fn ack(&self, id: BarrierId, verdict: AckVerdict) -> bool {
        self.take_running(id)
            .is_some_and(|tx| tx.try_send(Message::Ack(verdict)).is_ok())
    }

    /// Settle the running barrier `id` as `Failed("cancelled")` and wake its waiter.
    pub fn cancel(&self, id: BarrierId) -> bool {
        self.take_running(id)
            .is_some_and(|tx| tx.try_send(Message::Cancelled).is_ok())
    }

    /// Record how a barrier ended. A success clears an older failure: once everything is on disk,
    /// nothing may skip the barrier. Also the way to release a claimed ticket that never ran.
    pub fn settle(&self, reason: BarrierReason, result: &BarrierResult) {
        let mut st = self.lock();
        st.claimed = false;
        st.running = None;
        st.last_failure = match result {
            BarrierResult::Flushed => None,
            BarrierResult::Failed(_) | BarrierResult::NoAcknowledgement => {
                Some((reason, Instant::now()))
            }
        };
    }

    /// Coordinate one barrier: ask the frontend to flush (`request_flush`, given the id to echo),
    /// wait up to `timeout` for the ack, check its claims with `verify`, and settle.
    ///
    /// Blocks. Call it from a dedicated worker thread — never from the ordered `UserCommand` lane
    /// and never from the main thread (the webview needs the run loop to answer).
    pub fn run(
        &self,
        ticket: Ticket,
        reason: BarrierReason,
        request_flush: impl FnOnce(BarrierId) -> Result<(), String>,
        verify: impl Fn(&[SavedRevision]) -> Result<(), String>,
        timeout: Duration,
    ) -> BarrierResult {
        let Ticket { id, rx } = ticket;
        let result = match request_flush(id) {
            Err(error) => BarrierResult::Failed(format!("could not request a flush: {error}")),
            Ok(()) => match rx.recv_timeout(timeout) {
                Ok(Message::Ack(AckVerdict { ok: false, .. })) => BarrierResult::Failed(
                    "Develop edits could not be saved to disk. Check the disk, then try again."
                        .to_string(),
                ),
                Ok(Message::Ack(AckVerdict { ok: true, saved })) => match verify(&saved) {
                    Ok(()) => BarrierResult::Flushed,
                    Err(message) => BarrierResult::Failed(message),
                },
                Ok(Message::Cancelled) => BarrierResult::Failed("cancelled".to_string()),
                Err(RecvTimeoutError::Timeout) => BarrierResult::NoAcknowledgement,
                Err(RecvTimeoutError::Disconnected) => {
                    BarrierResult::Failed("save barrier dropped".to_string())
                }
            },
        };
        self.settle(reason, &result);
        result
    }
}

/// Check every claimed revision against the catalog: the stored revision must be at least the
/// claimed one. A missing row or a lower revision fails, naming the image.
pub fn verify_revisions(conn: &Connection, saved: &[SavedRevision]) -> Result<(), String> {
    for claim in saved {
        let stored: Option<i64> = conn
            .query_row(
                "SELECT revision FROM edits WHERE image_id = :image_id",
                named_params! { ":image_id": claim.image_id },
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| format!("could not verify the save of image {}: {e}", claim.image_id))?;
        match stored {
            Some(revision) if revision >= claim.revision => {}
            Some(revision) => {
                return Err(format!(
                    "image {} edit revision {} was reported saved but the catalog holds {}",
                    claim.image_id, claim.revision, revision
                ))
            }
            None => {
                return Err(format!(
                    "image {} edit revision {} was reported saved but the catalog has no edit",
                    claim.image_id, claim.revision
                ))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(b: &SaveBarrier, reason: BarrierReason) -> Ticket {
        match b.begin(reason) {
            Begin::Start(t) => t,
            _ => panic!("expected the barrier to start"),
        }
    }

    fn silent(b: &SaveBarrier, reason: BarrierReason) -> BarrierResult {
        let t = start(b, reason);
        b.run(t, reason, |_| Ok(()), |_| Ok(()), Duration::from_millis(10))
    }

    #[test]
    fn a_second_request_is_held_while_a_barrier_runs() {
        let b = SaveBarrier::new();
        let _t = start(&b, BarrierReason::Quit);
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Hold));
    }

    #[test]
    fn ids_are_monotonic_and_stray_acks_are_ignored() {
        let b = SaveBarrier::new();
        let t1 = start(&b, BarrierReason::Quit);
        assert!(!b.ack(BarrierId(t1.id.0 + 1), AckVerdict::ok(vec![])));
        assert!(b.ack(t1.id, AckVerdict::ok(vec![])));
        // Already answered: a second ack for the same id is a no-op.
        assert!(!b.ack(t1.id, AckVerdict::failed()));
        let first = t1.id;
        let r = b.run(
            t1,
            BarrierReason::Quit,
            |_| Ok(()),
            |_| Ok(()),
            Duration::from_millis(50),
        );
        assert_eq!(r, BarrierResult::Flushed);
        let t2 = start(&b, BarrierReason::Quit);
        assert!(t2.id.0 > first.0);
    }

    #[test]
    fn silence_is_no_acknowledgement_and_a_repeat_within_grace_discards_once() {
        let b = SaveBarrier::new();
        assert_eq!(
            silent(&b, BarrierReason::Quit),
            BarrierResult::NoAcknowledgement
        );
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Proceed));
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
    }

    #[test]
    fn grace_does_not_cross_barrier_reasons() {
        let b = SaveBarrier::new();
        silent(&b, BarrierReason::UpdaterRestart);
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));

        let b = SaveBarrier::new();
        silent(&b, BarrierReason::Quit);
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Proceed));
        assert!(matches!(
            b.begin(BarrierReason::UpdaterRestart),
            Begin::Start(_)
        ));
    }

    #[test]
    fn a_flushed_barrier_clears_an_older_failure() {
        let b = SaveBarrier::new();
        silent(&b, BarrierReason::UpdaterRestart);
        // A quit flushes in between: with everything on disk, the restart must get the barrier.
        let t = start(&b, BarrierReason::Quit);
        b.ack(t.id, AckVerdict::ok(vec![]));
        assert!(b
            .run(t, BarrierReason::Quit, |_| Ok(()), |_| Ok(()), ACK_TIMEOUT)
            .is_safe());
        assert!(matches!(
            b.begin(BarrierReason::UpdaterRestart),
            Begin::Start(_)
        ));
    }

    #[test]
    fn only_a_flush_counts_as_safe_to_leave() {
        assert!(BarrierResult::Flushed.is_safe());
        assert!(!BarrierResult::Failed("full disk".into()).is_safe());
        assert!(!BarrierResult::NoAcknowledgement.is_safe());
    }

    #[test]
    fn a_failed_flush_request_fails_the_barrier() {
        let b = SaveBarrier::new();
        let t = start(&b, BarrierReason::Quit);
        let r = b.run(
            t,
            BarrierReason::Quit,
            |_| Err("no webview".into()),
            |_| Ok(()),
            Duration::from_millis(10),
        );
        assert!(matches!(r, BarrierResult::Failed(m) if m.contains("no webview")));
    }
}
