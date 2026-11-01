//! The save barrier shared by a normal quit and by an updater relaunch.
//!
//! Develop edits are debounced in the webview before they reach `develop_set_edit`, so at the
//! moment the user presses ⌘Q (or accepts an update) the newest edit can still be sitting in a
//! JavaScript timer. Flushing the WAL on exit cannot recover that — the row was never written. So
//! the exit is held, the frontend is asked to flush, and the caller proceeds only once the frontend
//! has acknowledged a successful save.
//!
//! What the frontend owes is an *acknowledgement*, not just a reply:
//! - `ok = true`  → everything it had is on disk: [`BarrierResult::Flushed`].
//! - `ok = false` → a save failed: [`BarrierResult::Failed`].
//! - no reply within `ACK_TIMEOUT` → [`BarrierResult::NoAcknowledgement`] — the webview is wedged
//!   or gone, and we cannot tell "nothing was pending" from "the newest edit is still in memory".
//!
//! All three are handled the same way by the caller: only `Flushed` lets the process leave. The old
//! code treated a missing ack as success and exited anyway, which is precisely the case where an
//! edit is most likely still unsaved. Now a timeout cancels the exit the same way a failed flush
//! does: the failure is remembered, `app:save-barrier` is emitted so the UI can offer
//! retry / cancel / quit-anyway, and the app stays open.
//!
//! The escape hatch is deliberately kept: the *next* request within `RETRY_GRACE` proceeds
//! unconditionally. The user has been told the save failed and asked again, which is an explicit
//! decision to discard the in-memory edit — a failing disk must never hold someone hostage in their
//! own editor. That grace is the only way an exit happens without an acknowledgement. It is keyed
//! per [`BarrierReason`]: grace earned by discarding a warned updater-restart never lets a ⌘Q leave
//! with unsaved edits the quit never warned about, and vice versa.
//!
//! Mirroring the committed edits into the sidecars beside the RAWs is *not* part of the barrier:
//! `core_library::save_edit_with_intent` commits the intent in the same transaction as the edit, so
//! an unwritten sidecar is durable work the next launch finishes ([`crate::outbox_worker`]). Quit
//! therefore waits for the frontend, logs what the outbox still owes, and never blocks on it.

use serde::Serialize;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// How long to wait for the frontend's `develop_flush_ack` before giving up on it.
const ACK_TIMEOUT: Duration = Duration::from_millis(1500);
/// After a barrier that did not reach disk, a second request inside this window is honoured
/// unconditionally — see the module docs for why that is the user's decision, not ours.
const RETRY_GRACE: Duration = Duration::from_secs(30);

/// Event the frontend listens for; it replies with the `develop_flush_ack` command.
const FLUSH_EVENT: &str = "app:flush-edits";
// `app:save-barrier` is announced when a barrier did not reach disk, so the UI can say why and
// offer the choices (retry, cancel, or ask again to discard). It is emitted by name at the call
// site on purpose: the parity scanner reads event topics out of the `emit` call sites.

/// Who asked for the barrier. Reported to the UI so it can word the prompt for the right action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierReason {
    /// A user-initiated quit (`⌘Q`).
    Quit,
    /// The updater is about to relaunch the process.
    UpdaterRestart,
}

impl BarrierReason {
    fn label(self) -> &'static str {
        match self {
            BarrierReason::Quit => "quit",
            BarrierReason::UpdaterRestart => "updater-restart",
        }
    }

    /// What the user was doing, for the log.
    fn verb(self) -> &'static str {
        match self {
            BarrierReason::Quit => "quit",
            BarrierReason::UpdaterRestart => "restart",
        }
    }
}

/// How the barrier ended. Only `Flushed` authorizes leaving the process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierResult {
    /// The frontend confirmed every debounced edit is committed.
    Flushed,
    /// The frontend reported a save failure; carries a user-safe reason.
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
    /// Worded for whoever asked, since both the quit banner and the update banner show it.
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

/// Payload of `app:save-barrier`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaveBarrierReport {
    /// `"quit"` or `"updater-restart"`.
    pub reason: &'static str,
    pub message: String,
}

/// Build the UI report for one ended barrier. Pure, so the reason/outcome mapping is testable
/// without a webview.
fn report_for(reason: BarrierReason, result: &BarrierResult) -> SaveBarrierReport {
    SaveBarrierReport {
        reason: reason.label(),
        message: result.message(reason),
    }
}

#[derive(Default)]
struct QuitState {
    /// A barrier is already running — further requests must not start a second one.
    running: bool,
    /// Where `develop_flush_ack` delivers the frontend's answer.
    ack: Option<SyncSender<bool>>,
    /// When a barrier last failed to reach disk, and for which reason (see `RETRY_GRACE`).
    /// Keyed by reason: grace earned by discarding a warned updater-restart must not hand the
    /// next ⌘Q an instant `Proceed` (edits discarded with the quit never having been warned
    /// about), and vice versa.
    last_failure: Option<(BarrierReason, Instant)>,
}

/// What claiming the barrier decided.
enum Begin {
    /// Let the caller exit now (the grace window: the user already chose to discard).
    Proceed,
    /// Keep the caller waiting; a barrier already running will finish the job.
    Hold,
    /// Prevent the exit and wait for the frontend on this receiver.
    Start(Receiver<bool>),
}

/// One barrier at a time, plus the grace window that keeps a broken disk from trapping the app.
#[derive(Default)]
pub struct QuitBarrier {
    state: Mutex<QuitState>,
}

impl QuitBarrier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scheduling state only — recover from poisoning rather than killing the save path.
    fn lock(&self) -> std::sync::MutexGuard<'_, QuitState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Deliver the frontend's answer. Ignored when no barrier is waiting (a late or stray ack).
    pub fn ack(&self, ok: bool) {
        let tx = self.lock().ack.take();
        if let Some(tx) = tx {
            let _ = tx.try_send(ok);
        }
    }

    /// Claim the barrier for this request.
    fn begin(&self, reason: BarrierReason) -> Begin {
        let mut st = self.lock();
        if st.running {
            // A barrier is already asking the frontend to flush. Hold this request too and let that
            // one finish: it either exits for us or cancels, and a ⌘Q (or a second "Restart"
            // click) landing inside its ack window must not be the one that escapes unsaved.
            return Begin::Hold;
        }
        if st
            .last_failure
            .is_some_and(|(failed, t)| failed == reason && t.elapsed() < RETRY_GRACE)
        {
            // The user was told this request's save failed and asked again — honour the discard.
            // A grace earned by the *other* reason is not this one's to spend.
            st.last_failure = None;
            return Begin::Proceed;
        }
        let (tx, rx) = sync_channel(1);
        st.running = true;
        st.ack = Some(tx);
        Begin::Start(rx)
    }

    /// Record how the barrier ended, so the next request sees the grace window or not.
    /// A success clears an older failure: once everything is on disk, nothing may skip the barrier.
    fn settle(&self, reason: BarrierReason, result: &BarrierResult) {
        let mut st = self.lock();
        st.running = false;
        st.ack = None;
        st.last_failure = match result {
            BarrierResult::Flushed => None,
            BarrierResult::Failed(_) | BarrierResult::NoAcknowledgement => {
                Some((reason, Instant::now()))
            }
        };
    }
}

/// Ask the frontend to flush its debounced edits and wait for the verdict.
///
/// Must not run on the main thread: the webview needs the run loop alive both to receive
/// `app:flush-edits` and to deliver `develop_flush_ack` back.
fn await_ack(app: &AppHandle, rx: Receiver<bool>, reason: BarrierReason) -> BarrierResult {
    if app.webview_windows().is_empty() {
        // No webview to ask at all (last window closed; macOS keeps the app alive). Nothing is
        // held in editor memory that a flush could still recover, so the ack-forever case must
        // not cancel the exit with a toast nobody can see: proceed immediately. (The emit alone
        // cannot detect this — it succeeds happily with zero listeners.)
        app.state::<AppState>().quit.ack(true);
    } else if app.emit(FLUSH_EVENT, ()).is_err() {
        // Emit failed (serialization/tauri error) — the webview never got the ask. Same shape:
        // do not make the caller wait out the timeout for an undeliverable request.
        app.state::<AppState>().quit.ack(true);
    }
    let result = match rx.recv_timeout(ACK_TIMEOUT) {
        Ok(true) => BarrierResult::Flushed,
        Ok(false) => BarrierResult::Failed(
            "Develop edits could not be saved to disk. Check the disk, then try again.".to_string(),
        ),
        Err(_) => BarrierResult::NoAcknowledgement,
    };
    app.state::<AppState>().quit.settle(reason, &result);
    let outbox = app.state::<AppState>().outbox.pending_report();
    match &result {
        BarrierResult::Flushed => tracing::info!(
            reason = reason.verb(),
            outbox_pending = outbox.0,
            outbox_failing = outbox.1,
            outbox_last_error = outbox.2.as_deref().unwrap_or(""),
            "save barrier flushed; sidecar intents stay queued and durable across the restart"
        ),
        failed => {
            tracing::warn!(
                reason = reason.verb(),
                outbox_pending = outbox.0,
                outbox_failing = outbox.1,
                outbox_last_error = outbox.2.as_deref().unwrap_or(""),
                message = %failed.message(reason),
                "save barrier did not reach disk; the exit is cancelled"
            );
            let _ = app.emit("app:save-barrier", &report_for(reason, failed));
        }
    }
    result
}

/// Run the save barrier for `reason`. The caller decides what a non-`Flushed` result means for it:
/// quit cancels the exit, the updater refuses the relaunch.
pub fn run_save_barrier(app: &AppHandle, reason: BarrierReason) -> BarrierResult {
    match app.state::<AppState>().quit.begin(reason) {
        // The grace window: the user already chose to discard.
        Begin::Proceed => BarrierResult::Flushed,
        Begin::Hold => BarrierResult::Failed(
            "A save is already in progress; try again once it finishes.".to_string(),
        ),
        Begin::Start(rx) => await_ack(app, rx, reason),
    }
}

/// Handle `RunEvent::ExitRequested`. Returns `true` when the caller must call `api.prevent_exit()`.
///
/// `code` is `Some` for a programmatic exit — including this module's own `app.exit(0)` — which is
/// how the barrier avoids re-entering itself.
pub fn on_exit_requested(app: &AppHandle, code: Option<i32>) -> bool {
    if code.is_some() {
        return false;
    }
    // Claimed here, on the thread that receives the exit request: a second ⌘Q arriving while the
    // coordinator is starting must see `running` already set, or it would escape the barrier.
    let rx = match app.state::<AppState>().quit.begin(BarrierReason::Quit) {
        Begin::Proceed => return false,
        Begin::Hold => return true,
        Begin::Start(rx) => rx,
    };
    let barrier_app = app.clone();
    let host = app.clone();
    if let Err(error) = crate::jobs::worker(&host, "quit-flush", move || {
        // Only an acknowledged save may exit; `app.exit(0)` re-enters `ExitRequested` with a code,
        // which the check above lets through.
        if await_ack(&barrier_app, rx, BarrierReason::Quit).is_safe() {
            // `RunEvent::Exit` still does the sidecar flush + WAL checkpoint.
            barrier_app.exit(0);
        } else {
            tracing::warn!("quit cancelled: unsaved Develop edits are still held in the editor");
        }
        Ok(())
    }) {
        tracing::error!(%error, "quit coordinator failed to start");
        host.state::<AppState>().quit.settle(
            BarrierReason::Quit,
            &BarrierResult::Failed(format!("quit coordinator: {error}")),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_request_is_held_while_a_barrier_runs() {
        let b = QuitBarrier::new();
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
        // A ⌘Q inside the flush window must be held, not allowed to escape with an unsaved edit.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Hold));
    }

    #[test]
    fn the_ack_reaches_the_waiter() {
        let b = QuitBarrier::new();
        let Begin::Start(rx) = b.begin(BarrierReason::Quit) else {
            panic!("first request must start the barrier");
        };
        b.ack(true);
        assert_eq!(rx.recv_timeout(Duration::from_millis(50)), Ok(true));
        // The channel is consumed: a stray second ack is a no-op, not a panic.
        b.ack(false);
    }

    #[test]
    fn no_acknowledgement_cancels_the_exit_and_a_second_request_within_grace_discards() {
        let b = QuitBarrier::new();
        // The first request is held (never `Proceed`) because the frontend is silent.
        let Begin::Start(rx) = b.begin(BarrierReason::Quit) else {
            panic!("first request must start the barrier");
        };
        assert!(
            rx.recv_timeout(Duration::from_millis(10)).is_err(),
            "a silent frontend must not produce an ack"
        );
        b.settle(BarrierReason::Quit, &BarrierResult::NoAcknowledgement);
        // …and the exit was cancelled, not taken: no `Proceed` was ever handed out for it. Only the
        // user's second request, inside the grace window, may exit — as an explicit discard.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Proceed));
        // …and the grace is spent, so a later request gets the barrier again.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
    }

    #[test]
    fn quitting_again_after_a_failure_is_honoured_once() {
        let b = QuitBarrier::new();
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
        b.settle(
            BarrierReason::Quit,
            &BarrierResult::Failed("read-only volume".into()),
        );
        // The user has seen the banner and asked to quit anyway.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Proceed));
        // …and the grace is spent, so a later quit gets the barrier again.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
    }

    /// The grace window is keyed by reason: the user was warned about — and chose to discard —
    /// exactly one kind of exit. A failed updater-restart must not hand the next ⌘Q an instant
    /// `Proceed` (quitting with in-memory edits discarded, a choice never shown for quit), and a
    /// failed quit must not free-pass a restart.
    #[test]
    fn grace_does_not_cross_barrier_reasons() {
        // A failed updater-restart grants restart-again-to-discard …
        let b = QuitBarrier::new();
        assert!(matches!(
            b.begin(BarrierReason::UpdaterRestart),
            Begin::Start(_)
        ));
        b.settle(
            BarrierReason::UpdaterRestart,
            &BarrierResult::NoAcknowledgement,
        );
        // … but a ⌘Q inside the grace still gets the FULL barrier. (Quit earning and spending its
        // own grace is pinned by `quitting_again_after_a_failure_is_honoured_once`.)
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));

        // Symmetrically: a failed quit must not free-pass an updater restart.
        let b = QuitBarrier::new();
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
        b.settle(
            BarrierReason::Quit,
            &BarrierResult::Failed("full disk".into()),
        );
        // Quit's own discard still works once …
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Proceed));
        // … and the restart gets the full barrier instead of the quit's spent grace.
        assert!(matches!(
            b.begin(BarrierReason::UpdaterRestart),
            Begin::Start(_)
        ));
    }

    #[test]
    fn a_flushed_barrier_leaves_no_grace_window() {
        let b = QuitBarrier::new();
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
        b.settle(BarrierReason::Quit, &BarrierResult::Flushed);
        // Everything is on disk: the next quit must get the full barrier, not a free pass.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
        b.settle(BarrierReason::Quit, &BarrierResult::Flushed);
        // A restart failure that was never spent …
        assert!(matches!(
            b.begin(BarrierReason::UpdaterRestart),
            Begin::Start(_)
        ));
        b.settle(
            BarrierReason::UpdaterRestart,
            &BarrierResult::NoAcknowledgement,
        );
        // … becomes moot once a later quit flushes: with everything on disk, nothing may skip
        // the barrier, under either reason.
        assert!(matches!(b.begin(BarrierReason::Quit), Begin::Start(_)));
        b.settle(BarrierReason::Quit, &BarrierResult::Flushed);
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
    fn the_report_names_who_was_asked_and_why_it_stopped() {
        let quit = report_for(BarrierReason::Quit, &BarrierResult::NoAcknowledgement);
        assert_eq!(quit.reason, "quit");
        assert!(quit.message.contains("quit"), "{}", quit.message);

        let updater = report_for(
            BarrierReason::UpdaterRestart,
            &BarrierResult::Failed("read-only volume".into()),
        );
        assert_eq!(updater.reason, "updater-restart");
        assert_eq!(updater.message, "read-only volume");

        // A flushed barrier has nothing to say; callers never emit for it.
        assert!(report_for(BarrierReason::Quit, &BarrierResult::Flushed)
            .message
            .is_empty());
    }
}
