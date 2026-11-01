//! Drains the durable sidecar-mirror intent queue (`sidecar_outbox`, migrations 026/027).
//!
//! [`crate::sidecar_queue`] answers "this image's sidecar is stale *right now*": its dirty set is
//! in memory, so a crash between the catalog commit and the file write loses the intent and the
//! sidecar stays behind the catalog forever. `core_library::save_edit_with_intent` commits the
//! intent in the same transaction as the edit, so the knowledge survives; this worker is the half
//! that acts on it — one thread that takes the catalog lock briefly to read pending intents,
//! performs each fenced write with the lock released ([`crate::sidecar_write::write_one`]), and
//! reports the outcome back to the catalog ([`core_library::complete_outbox`] /
//! [`core_library::fail_outbox`] / [`core_library::defer_outbox`]).
//!
//! Properties the rest of the quit/save story depends on:
//! - **Fair drain.** A pass walks ALL due rows by keyset, `PAGE` at a time, so a block of
//!   unavailable or permission-failing rows can never pin the head of the queue and starve a
//!   healthy row behind them. Every attempted row either leaves the queue or is pushed to a future
//!   `next_attempt_at`.
//! - **Durable eligibility.** Retry pacing lives in `sidecar_outbox.next_attempt_at` (unix ms, from
//!   an injectable [`OutboxClock`]), not in memory: a poll tick or a `notify()` before a row is due
//!   attempts nothing, and a restart (which resets the schedule to "now") retries everything once.
//! - **Restart recovery.** The first pass after startup drains whatever the previous run left, so
//!   a card that was unplugged mid-write gets mirrored on the next launch without the user doing
//!   anything. Attempt counts and `last_error` survive; only the waiting is forgotten.
//! - **Responsive stop.** `stop` is checked before every item, not just between passes.
//! - **Backpressure-free.** A pending intent is *not* an error and never blocks quit: the row is
//!   still owed a write, and the next launch will perform it. [`OutboxHandle::pending_report`] only
//!   informs the log and the UI — `failing` (an attempt that hit the filesystem) is what is worth
//!   surfacing.

use crate::sidecar_write::{production_write, safe_write_error, write_one, SidecarOutcome};
use crate::state::AppState;
use core_db::Db;
use core_library::{
    complete_outbox, defer_outbox, fail_outbox, next_outbox_due, pending_outbox_keyed,
    reset_outbox_schedule, OutboxCompletion, SidecarWrite,
};
use core_runtime::publication::PublicationGate;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Manager};

/// Intents read per query. One image can only have one row, so this bounds how long a page read
/// holds the catalog lock; the pass keeps paging until a page comes back short.
const PAGE: usize = 64;
/// Same pacing as the coalescing sidecar queue: a failing card is retried often enough to catch a
/// transient condition (remount, space freed) and rarely enough not to spin on it.
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// The worker parks on a condvar, so the only thing that wakes it for sure is a signal it owns.
/// Poll this often at most so a shutdown that races a park cannot strand the thread (and with it
/// `jobs::shutdown`, which joins every managed worker). Waking is cheap: a wake before anything is
/// due drains nothing.
const POLL: Duration = Duration::from_secs(1);

/// Wall-clock source for retry eligibility, injectable so tests drive time explicitly.
pub trait OutboxClock: Send + Sync {
    /// Unix milliseconds.
    fn now_ms(&self) -> i64;
}

/// The real clock.
pub struct SystemClock;

impl OutboxClock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }
}

/// Exponential backoff for the `n`-th failed attempt of one intent.
fn backoff(attempts: i64) -> Duration {
    let base = BACKOFF_BASE.as_millis() as u64;
    let shift = u32::try_from(attempts.max(1) - 1).unwrap_or(u32::MAX);
    Duration::from_millis(base.saturating_mul(2u64.saturating_pow(shift))).min(BACKOFF_CAP)
}

/// `now + delay` in unix ms, saturating.
fn after(now_ms: i64, delay: Duration) -> i64 {
    now_ms.saturating_add(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX))
}

/// What one drain pass did. Returned by [`drain_once`] so the log line and the worker's pacing are
/// both derived from observed work.
#[derive(Debug, Default)]
pub(crate) struct DrainReport {
    /// Intents taken off the queue and attempted.
    pub attempted: usize,
    /// Intents whose write succeeded and whose `mirrored_revision` now matches.
    pub mirrored: usize,
    /// Intents settled without a write of ours: superseded, or the row/edit is gone.
    pub dropped: usize,
    /// Intents kept for another pass (identity churn, source not present, or a failed write).
    pub kept: usize,
    /// Earliest `next_attempt_at` still in the queue after the pass (0 = something is due now,
    /// e.g. a requeued relink); `None` when the queue is empty.
    pub next_due: Option<i64>,
    /// Path-masked summary of the most recent write failure seen this pass.
    pub last_error: Option<String>,
    /// `stop()` ended the pass early; the unvisited rows are still queued.
    pub interrupted: bool,
}

/// One pass over the queue: write every DUE intent, report each verdict. Catalog-only reads are
/// brief — the fenced snapshot and the bytes-on-disk happen inside `write_one`, which holds no
/// catalog guard (or gate) while it serializes and stages the file.
///
/// Paging is by keyset over `(next_attempt_at, enqueued_at, image_id)` and eligibility is taken
/// from `clock` once per pass, so a row deferred during the pass is never revisited in it and a
/// stuck head-of-queue cannot hide the healthy rows behind it. `stop` is checked before every item.
///
/// `write` is the staging seam that keeps this testable: production passes
/// [`crate::sidecar_write::production_write`], tests pass a closure that fails on purpose.
/// `Err` is reserved for a catalog that cannot be locked at all; every per-image problem is a
/// counted outcome, never a reason to stop draining.
pub(crate) fn drain_once(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    clock: &dyn OutboxClock,
    stop: &dyn Fn() -> bool,
) -> Result<DrainReport, String> {
    drain_with(gate, db, write, clock, stop, &|| {})
}

/// [`drain_once`] with a test seam: `after_write` runs between a successful write and its
/// completion report (the window in which a file can be renamed under the write).
pub(crate) fn drain_with(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    clock: &dyn OutboxClock,
    stop: &dyn Fn() -> bool,
    after_write: &dyn Fn(),
) -> Result<DrainReport, String> {
    let eligible_at = clock.now_ms();
    let mut report = DrainReport::default();
    let mut cursor = None;
    'pages: loop {
        if stop() {
            report.interrupted = true;
            break;
        }
        let page = {
            let db = db.lock().map_err(|_| "catalog lock poisoned")?;
            pending_outbox_keyed(&db.conn, eligible_at, cursor, PAGE)
                .map_err(|e| safe_write_error(&e.to_string()))?
        };
        let short = page.len() < PAGE;
        for (item, position) in page {
            if stop() {
                report.interrupted = true;
                break 'pages;
            }
            cursor = Some(position);
            report.attempted += 1;
            attempt(gate, db, write, clock, &item, &mut report, after_write)?;
        }
        if short {
            break;
        }
    }
    let db = db.lock().map_err(|_| "catalog lock poisoned")?;
    report.next_due = next_outbox_due(&db.conn).map_err(|e| safe_write_error(&e.to_string()))?;
    Ok(report)
}

/// Write one intent and record the verdict in the catalog.
fn attempt(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    clock: &dyn OutboxClock,
    item: &core_library::OutboxItem,
    report: &mut DrainReport,
    after_write: &dyn Fn(),
) -> Result<(), String> {
    let id = item.image_id;
    let rev = item.edit_revision;
    match write_one(gate, db, id, write, &|| {})? {
        SidecarOutcome::Written => {
            after_write();
            // Any answer but `Completed` is still progress: `Superseded` leaves the newer intent
            // queued (its own write proves itself), `Requeued` retries the relinked row against its
            // current identity, `SourceChanged` keeps the intent for a row that is not present right
            // now, and `Gone` cascaded a row that can never be satisfied. Only the write we actually
            // made is claimed.
            let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
            let outcome = complete_outbox(&catalog.conn, item)
                .map_err(|e| safe_write_error(&e.to_string()))?;
            match outcome {
                OutboxCompletion::Completed => report.mirrored += 1,
                // The file moved/was renamed mid-write and the intent was re-pointed at the row's
                // current identity (with `next_attempt_at = 0`): the write that just succeeded
                // describes a file that is no longer this image's, so the new path is owed an
                // attempt NOW — no backoff. Register the row's current identity in the publication
                // gate first (under the source guard, like the watcher's commits): without it the
                // retry's `capture` keeps failing on the stale key and the intent spins on Retry.
                // Lock order: gate before catalog, so release the catalog guard first.
                OutboxCompletion::Requeued => {
                    drop(catalog);
                    requeue_identity(gate, db, id)?;
                    report.kept += 1;
                }
                // Same revision, different file, row not present: keep the intent, park it.
                OutboxCompletion::SourceChanged => {
                    defer_outbox(
                        &catalog.conn,
                        id,
                        rev,
                        after(clock.now_ms(), BACKOFF_CAP),
                        Some("source not present"),
                    )
                    .map_err(|e| safe_write_error(&e.to_string()))?;
                    report.kept += 1;
                }
                other => {
                    tracing::debug!(
                        image_id = id,
                        ?other,
                        "sidecar mirror already settled elsewhere"
                    );
                    report.dropped += 1;
                }
            }
        }
        // The row is deleted: its intent went with it (FK cascade), so there is nothing left to
        // report and nothing left to write.
        SidecarOutcome::Gone => {
            tracing::debug!(image_id = id, "sidecar intent orphaned by a deleted source");
            report.dropped += 1;
        }
        // The row exists but its source is not present (unplugged card). The intent is still owed:
        // never delete it, never charge an attempt, and park it so it cannot be re-selected before
        // the cap elapses. A relink / remount resets the schedule and brings it back at once.
        SidecarOutcome::Unavailable => {
            let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
            defer_outbox(
                &catalog.conn,
                id,
                rev,
                after(clock.now_ms(), BACKOFF_CAP),
                Some("source not present"),
            )
            .map_err(|e| safe_write_error(&e.to_string()))?;
            report.kept += 1;
        }
        // The catalog moved while we were writing: re-snapshot after a short pause. No attempt is
        // charged — this is not a failed write, and charging one for someone else's mutation would
        // age a healthy intent out of the backoff that is meant for a broken volume.
        SidecarOutcome::Retry => {
            let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
            defer_outbox(
                &catalog.conn,
                id,
                rev,
                after(clock.now_ms(), backoff(item.attempts)),
                None,
            )
            .map_err(|e| safe_write_error(&e.to_string()))?;
            report.kept += 1;
        }
        // A real filesystem failure: keep the intent, record why, back off.
        SidecarOutcome::Failed(error) => {
            let masked = safe_write_error(&error);
            tracing::warn!(image_id = id, error = %masked, "outbox sidecar write failed; keeping the intent");
            let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
            fail_outbox(
                &catalog.conn,
                id,
                rev,
                &masked,
                after(clock.now_ms(), backoff(item.attempts + 1)),
            )
            .map_err(|e| safe_write_error(&e.to_string()))?;
            report.kept += 1;
            report.last_error = Some(masked);
        }
    }
    Ok(())
}

/// Register a requeued row's current identity in the publication gate (gate before catalog).
fn requeue_identity(gate: &PublicationGate, db: &Mutex<Db>, image_id: i64) -> Result<(), String> {
    let mut source = gate.source_guard();
    let row = {
        let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
        core_library::image_by_id(&catalog.conn, image_id)
            .map_err(|e| safe_write_error(&e.to_string()))?
    };
    if let Some(row) = row {
        source.replace_source(
            core_model::identity::ImageId(image_id),
            core_runtime::publication::SourceKey {
                hash: row.content_hash,
                path: row.path,
            },
        );
        tracing::debug!(
            image_id,
            "sidecar intent re-queued at the row's current identity"
        );
    }
    Ok(())
}

struct Inner {
    db: Arc<Mutex<Db>>,
    clock: Arc<dyn OutboxClock>,
    /// Producers (`develop_set_edit`) and shutdown wake the worker here.
    cv: Condvar,
    /// The lock the worker parks on; guards nothing but the wait.
    park: Mutex<()>,
    /// Set by [`OutboxHandle::shutdown`]; the worker then finishes its in-hand item and exits.
    stopping: AtomicBool,
}

/// Shared handle to the mirroring worker, stored in `AppState`.
#[derive(Clone)]
pub struct OutboxHandle {
    inner: Arc<Inner>,
}

impl OutboxHandle {
    pub fn new(db: Arc<Mutex<Db>>) -> Self {
        Self::with_clock(db, Arc::new(SystemClock))
    }

    /// Same, with the retry clock injected.
    pub fn with_clock(db: Arc<Mutex<Db>>, clock: Arc<dyn OutboxClock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                clock,
                cv: Condvar::new(),
                park: Mutex::new(()),
                stopping: AtomicBool::new(false),
            }),
        }
    }

    /// An intent was just committed; go look. Cheap and non-blocking, and safe to call while the
    /// catalog lock is held (this touches only the worker's own mutex).
    pub fn notify(&self) {
        // A fresh intent is committed with `next_attempt_at = 0`, so it is due the moment the
        // worker re-reads the catalog: a bare wake-up is correct (no flag to set) and a lost one is
        // bounded by `POLL`. A wake with nothing due drains nothing.
        self.inner.cv.notify_all();
    }

    /// `(still owed, failing, why)` for the quit/updater log line. A non-zero `pending` is normal
    /// (a write in flight) and never blocks an exit; `failing` is the one that means the disk said
    /// no. Reads the catalog directly so the answer cannot be a stale cache.
    pub fn pending_report(&self) -> (usize, usize, Option<String>) {
        let Ok(db) = self.inner.db.lock() else {
            return (0, 0, Some("catalog lock poisoned".to_string()));
        };
        let count = |sql: &str| -> usize {
            db.conn
                .query_row(sql, [], |r| r.get::<_, i64>(0))
                .unwrap_or(0) as usize
        };
        let pending = count("SELECT COUNT(*) FROM sidecar_outbox");
        let failing = count("SELECT COUNT(*) FROM sidecar_outbox WHERE attempts > 0");
        let last_error = db
            .conn
            .query_row(
                "SELECT last_error FROM sidecar_outbox
                  WHERE last_error IS NOT NULL
                  ORDER BY enqueued_at DESC, image_id LIMIT 1",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        (pending, failing, last_error)
    }

    /// Ask the worker to finish its in-hand item and exit. Safe to call more than once, and safe to
    /// never call: the poll interval bounds a missed wake-up.
    pub fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
    }

    fn stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::SeqCst)
    }

    /// Worker start: make every queued intent due now. A restart retries everything immediately
    /// (attempts / last_error are kept), rather than inheriting a backoff the previous run set.
    fn reset_schedule(&self) {
        let reset = self
            .inner
            .db
            .lock()
            .map_err(|_| "catalog lock poisoned".to_string())
            .and_then(|db| {
                reset_outbox_schedule(&db.conn, None).map_err(|e| safe_write_error(&e.to_string()))
            });
        if let Err(error) = reset {
            tracing::warn!(%error, "sidecar outbox schedule reset failed");
        }
    }

    /// One worker iteration: re-read when the earliest intent is due, drain only if it is due now,
    /// and return how long to wait before looking again. A wake before anything is due drains
    /// nothing. The wait is capped at `POLL` purely so a stop signal is noticed promptly.
    fn pass(
        &self,
        gate: &PublicationGate,
        write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
        stop: &dyn Fn() -> bool,
    ) -> Duration {
        let clock = &*self.inner.clock;
        match self.due_in_ms() {
            Ok(Some(0)) => match drain_once(gate, &self.inner.db, write, clock, stop) {
                Ok(report) => {
                    if report.attempted > 0 {
                        tracing::info!(
                            attempted = report.attempted,
                            mirrored = report.mirrored,
                            dropped = report.dropped,
                            kept = report.kept,
                            interrupted = report.interrupted,
                            last_error = report.last_error.as_deref().unwrap_or(""),
                            "sidecar outbox drained"
                        );
                    }
                }
                // A catalog we could not read: keep the intents (that is the whole point of the
                // table) and try again on the next wake-up rather than ending the worker.
                Err(error) => tracing::warn!(%error, "sidecar outbox pass failed"),
            },
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "sidecar outbox pass failed"),
        }
        match self.due_in_ms() {
            // Still due after a pass (a requeue, or `stop` interrupted it): come straight back
            // unless we are stopping, in which case the caller exits before waiting.
            Ok(Some(0)) => POLL.min(Duration::from_millis(50)),
            // An unreadable catalog (e.g. poisoned lock) is not fixed by spinning: back off to
            // the ordinary poll so the warning above is logged at most once a second.
            Err(_) => POLL,
            Ok(Some(ms)) => Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX)).min(POLL),
            Ok(None) => POLL,
        }
    }

    /// `None` when nothing is queued, else how many ms until the earliest intent is due (0 = now).
    fn due_in_ms(&self) -> Result<Option<i64>, String> {
        let db = self
            .inner
            .db
            .lock()
            .map_err(|_| "catalog lock poisoned".to_string())?;
        let due = next_outbox_due(&db.conn).map_err(|e| safe_write_error(&e.to_string()))?;
        Ok(due.map(|due| (due - self.inner.clock.now_ms()).max(0)))
    }

    /// Park until notified or `timeout` (never longer than `POLL`) elapses.
    fn wait(&self, timeout: Duration) {
        let guard = self
            .inner
            .park
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let waited = self.inner.cv.wait_timeout(guard, timeout.min(POLL));
        // Poisoning here means a panic happened while another thread held the park guard, which
        // cannot corrupt anything: the guard guards nothing but the wait.
        drop(waited.unwrap_or_else(std::sync::PoisonError::into_inner));
    }
}

/// Spawn the single mirroring worker (same place as the sidecar/thumbnail workers).
pub fn spawn_worker(app: AppHandle) {
    let handle = {
        let st = app.state::<AppState>();
        st.outbox.clone()
    };
    let worker_app = app.clone();
    if let Err(error) = crate::jobs::worker(&app, "outbox-worker", move || {
        handle.reset_schedule();
        let stop = || handle.stopping() || worker_app.state::<AppState>().workers.is_stopping();
        loop {
            // Finish the in-hand item, then leave. The supervisor's flag catches a quit whose
            // shutdown signal raced our park; `handle.stopping()` is the ordered path.
            if stop() {
                handle.shutdown();
                return Ok(());
            }
            let wait = {
                let st = worker_app.state::<AppState>();
                handle.pass(&st.publications, &production_write, &stop)
            };
            if stop() {
                handle.shutdown();
                return Ok(());
            }
            handle.wait(wait);
        }
    }) {
        tracing::error!(%error, "sidecar outbox worker failed to start");
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar_write::tests::Fix;
    use core_library::{
        mirrored_revision, pending_outbox, reset_outbox_schedule, save_edit_with_intent,
    };
    use std::sync::atomic::{AtomicI64, AtomicUsize};

    const PARAMS: &str = "{\"exposure\":0.5}";

    /// Test clock: time only moves when the test says so.
    struct FakeClock(AtomicI64);

    impl FakeClock {
        fn new() -> Self {
            Self(AtomicI64::new(1_000_000))
        }
        fn advance(&self, d: Duration) {
            self.0.fetch_add(d.as_millis() as i64, Ordering::SeqCst);
        }
    }

    impl OutboxClock for FakeClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn never() -> bool {
        false
    }

    fn drain(
        f: &Fix,
        write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
        c: &FakeClock,
    ) -> DrainReport {
        drain_once(&f.gate, &f.db, write, c, &never).unwrap()
    }

    /// The command's durable half: commit an edit plus its mirror intent.
    fn save(f: &Fix, id: i64) -> i64 {
        let db = f.db.lock().unwrap();
        save_edit_with_intent(&db.conn, id, 7, PARAMS, core_library::now_epoch()).unwrap()
    }

    fn owed(f: &Fix) -> Vec<core_library::OutboxItem> {
        pending_outbox(&f.db.lock().unwrap().conn, i64::MAX, None, 1024).unwrap()
    }

    fn row(f: &Fix, id: i64) -> core_library::OutboxItem {
        owed(f)
            .into_iter()
            .find(|r| r.image_id == id)
            .expect("intent still queued")
    }

    fn set_status(f: &Fix, id: i64, status: &str) {
        f.db.lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE images SET status = ?2 WHERE id = ?1",
                core_db::rusqlite::params![id, status],
            )
            .unwrap();
    }

    fn denied(_: &SidecarWrite) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }

    /// No staging litter anywhere in the fixture directory.
    fn no_temp_files(f: &Fix) -> bool {
        std::fs::read_dir(f.dir())
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
    }

    #[test]
    fn a_saved_edit_is_mirrored_and_the_intent_settles() {
        let f = Fix::new("outbox-mirror");
        let a = f.insert_image(1, "a.cr3");
        let revision = save(&f, a);

        let report = drain(&f, &production_write, &FakeClock::new());
        assert_eq!(report.attempted, 1);
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert_eq!(report.next_due, None, "a settled intent owes nothing");

        let sidecar = f.sidecar("a.cr3").expect("sidecar written beside the RAW");
        assert_eq!(
            sidecar.develop,
            Some(serde_json::from_str::<serde_json::Value>(PARAMS).unwrap()),
            "the sidecar carries the params the catalog just committed"
        );
        assert_eq!(sidecar.process_version, Some(7));
        assert!(owed(&f).is_empty(), "the intent is gone once mirrored");
        assert!(no_temp_files(&f), "the staged temp was renamed away");
        assert_eq!(
            mirrored_revision(&f.db.lock().unwrap().conn, a).unwrap(),
            Some((revision, revision)),
            "mirrored == revision is the durable proof"
        );
    }

    #[test]
    fn a_failed_write_keeps_the_intent_with_one_attempt() {
        let f = Fix::new("outbox-failure");
        let a = f.insert_image(2, "a.cr3");
        let revision = save(&f, a);
        let clock = FakeClock::new();

        let report = drain(&f, &denied, &clock);
        assert_eq!(report.kept, 1, "{report:?}");
        assert!(
            report.next_due.is_some_and(|due| due > clock.now_ms()),
            "a kept intent is timed in the future, not spun on: {report:?}"
        );
        assert!(
            report
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("denied")),
            "{report:?}"
        );

        let rows = owed(&f);
        assert_eq!(rows.len(), 1, "the intent survives the failed write");
        assert_eq!(rows[0].image_id, a);
        assert_eq!(rows[0].edit_revision, revision);
        assert_eq!(
            rows[0].attempts, 1,
            "the attempt is recorded, not swallowed"
        );
        assert!(rows[0]
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("denied")));
        assert_eq!(
            mirrored_revision(&f.db.lock().unwrap().conn, a).unwrap(),
            Some((revision, 0)),
            "nothing may be claimed mirrored while the write failed"
        );
        assert!(f.sidecar("a.cr3").is_none());
        assert!(no_temp_files(&f));
    }

    #[test]
    fn an_intent_left_by_the_previous_run_is_written_on_the_next_one() {
        let f = Fix::new("outbox-restart");
        let a = f.insert_image(3, "a.cr3");
        save(&f, a);
        // The previous run: the card was not writable, so the intent stayed queued (and backed off).
        let clock = FakeClock::new();
        drain(&f, &denied, &clock);
        assert!(f.sidecar("a.cr3").is_none());

        // A restart loses every in-memory queue; the catalog is all that is left. The worker
        // resets the schedule at start, so the backed-off row is retried immediately.
        let reopened = f.reopened();
        reset_outbox_schedule(&reopened.db.lock().unwrap().conn, None).unwrap();
        assert_eq!(
            row(&reopened, a).attempts,
            1,
            "attempts survive the restart"
        );
        let report = drain_once(
            &reopened.gate,
            &reopened.db,
            &production_write,
            &clock,
            &never,
        )
        .unwrap();
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert!(
            f.sidecar("a.cr3").is_some(),
            "the restart wrote the sidecar"
        );
        assert!(owed(&reopened).is_empty());
        assert_eq!(
            mirrored_revision(&reopened.db.lock().unwrap().conn, a).unwrap(),
            Some((1, 1))
        );
    }

    #[test]
    fn a_renamed_source_requeues_the_intent_and_retries_now() {
        let f = Fix::new("outbox-requeue");
        let a = f.insert_image(7, "a.cr3");
        let revision = save(&f, a);
        let clock = FakeClock::new();

        // The file is renamed between the fenced write succeeding and the outcome being reported:
        // `write_one` returns `Written` (its in-fence re-read already passed), and `complete_outbox`
        // finds a present row at a new path. The intent must survive re-pointed at that identity —
        // deleting it would strand the edit, since relink never re-queues — and the pass must owe
        // an immediate retry, not a backoff.
        let mid_write_db = f.db.clone();
        let rename_row = || {
            let db = mid_write_db.lock().unwrap();
            let old: String = db
                .conn
                .query_row("SELECT path FROM images WHERE id = ?1", [a], |r| r.get(0))
                .unwrap();
            db.conn
                .execute(
                    "UPDATE images SET path = ?2, original_filename = 'b.cr3' WHERE id = ?1",
                    core_db::rusqlite::params![a, old.replace("a.cr3", "b.cr3")],
                )
                .unwrap();
        };
        let report = drain_with(
            &f.gate,
            &f.db,
            &production_write,
            &clock,
            &never,
            &rename_row,
        )
        .unwrap();
        assert_eq!(report.attempted, 1);
        assert_eq!(report.mirrored, 0, "a moved file's write settles nothing");
        assert_eq!(report.kept, 1, "{report:?}");
        assert_eq!(
            report.next_due,
            Some(0),
            "a requeued intent retries now, not after a backoff"
        );
        let rows = owed(&f);
        assert_eq!(rows.len(), 1, "the intent is re-pointed, not deleted");
        assert!(
            rows[0].path.ends_with("b.cr3"),
            "intent follows the row: {}",
            rows[0].path
        );
        assert_eq!(rows[0].edit_revision, revision);
        assert_eq!(
            rows[0].attempts, 0,
            "old-path failures do not age the new file"
        );
        assert!(f.sidecar("b.cr3").is_none());

        // The immediate retry re-snapshots under a fresh fence and settles at the new identity.
        let report = drain(&f, &production_write, &clock);
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert!(owed(&f).is_empty());
        assert!(
            f.sidecar("b.cr3").is_some(),
            "the sidecar lands beside the moved file"
        );
        assert_eq!(
            mirrored_revision(&f.db.lock().unwrap().conn, a).unwrap(),
            Some((revision, revision))
        );
    }

    #[test]
    fn pending_report_counts_what_is_owed_and_names_the_failure() {
        let f = Fix::new("outbox-report");
        let a = f.insert_image(5, "a.cr3");
        let b = f.insert_image(6, "b.cr3");
        save(&f, a);
        save(&f, b);
        let handle = OutboxHandle::new(f.db.clone());
        // Committed, never attempted: owed, but nothing has failed yet.
        assert_eq!(handle.pending_report().0, 2);
        assert_eq!(handle.pending_report().1, 0);
        assert_eq!(handle.pending_report().2, None);

        let clock = FakeClock::new();
        drain(&f, &denied, &clock);
        let (pending, failing, last_error) = handle.pending_report();
        assert_eq!(pending, 2, "a failed write still owes a sidecar");
        assert_eq!(failing, 2, "both attempts hit the filesystem");
        assert!(
            last_error.as_deref().is_some_and(|e| e.contains("denied")),
            "{last_error:?}"
        );

        // The volume comes back and the backoff elapses: settling removes them from the report.
        clock.advance(BACKOFF_CAP);
        drain(&f, &production_write, &clock);
        assert_eq!(handle.pending_report(), (0, 0, None));
    }

    #[test]
    fn deleting_the_image_drops_the_intent_without_writing_anything() {
        let f = Fix::new("outbox-deleted");
        let a = f.insert_image(4, "a.cr3");
        save(&f, a);
        f.db.lock()
            .unwrap()
            .conn
            .execute("DELETE FROM images WHERE id = ?1", [a])
            .unwrap();

        let report = drain(&f, &production_write, &FakeClock::new());
        assert_eq!(
            report.attempted, 0,
            "the FK cascade took the intent with the row"
        );
        assert!(owed(&f).is_empty());
        assert!(
            f.sidecar("a.cr3").is_none(),
            "a deleted source never gets a sidecar"
        );
    }

    #[test]
    fn unavailable_sources_do_not_starve_a_healthy_row() {
        let f = Fix::new("outbox-unavailable");
        let clock = FakeClock::new();
        // More missing rows than one page, all older than the healthy one.
        let missing: Vec<i64> = (0..PAGE as u8)
            .map(|i| f.insert_image(i, &format!("m{i}.cr3")))
            .collect();
        for &id in &missing {
            save(&f, id);
            set_status(&f, id, "missing");
        }
        let healthy = f.insert_image(200, "healthy.cr3");
        save(&f, healthy);

        let report = drain(&f, &production_write, &clock);
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert!(f.sidecar("healthy.cr3").is_some(), "healthy row written");
        let rows = owed(&f);
        assert_eq!(
            rows.len(),
            PAGE,
            "missing-source intents are kept, never deleted"
        );
        assert!(rows
            .iter()
            .all(|r| r.next_attempt_at > clock.now_ms() && r.attempts == 0));

        // Nothing is due until the cap passes: a second wake-up attempts nothing.
        let again = drain(&f, &production_write, &clock);
        assert_eq!(again.attempted, 0, "{again:?}");
        assert!(no_temp_files(&f));
    }

    #[test]
    fn permission_failures_do_not_starve_a_healthy_row() {
        let f = Fix::new("outbox-permission");
        let clock = FakeClock::new();
        let bad: Vec<i64> = (0..PAGE as u8)
            .map(|i| f.insert_image(i, &format!("bad{i}.cr3")))
            .collect();
        for &id in &bad {
            save(&f, id);
        }
        let healthy = f.insert_image(200, "good.cr3");
        save(&f, healthy);

        // The staged temp is `.badN.cr3.json.<pid>-<n>.tmp`.
        let write = |w: &SidecarWrite| {
            if w.path.to_string_lossy().contains("/.bad") {
                denied(w)
            } else {
                production_write(w)
            }
        };
        let report = drain(&f, &write, &clock);
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert!(f.sidecar("good.cr3").is_some());
        let rows = owed(&f);
        assert_eq!(rows.len(), PAGE);
        assert!(rows.iter().all(|r| r.attempts == 1), "one attempt each");
    }

    #[test]
    fn wakeups_before_eligibility_attempt_nothing_and_time_makes_rows_due_again() {
        let f = Fix::new("outbox-backoff");
        let a = f.insert_image(1, "a.cr3");
        save(&f, a);
        let clock = FakeClock::new();
        let calls = AtomicUsize::new(0);
        let write = |w: &SidecarWrite| {
            calls.fetch_add(1, Ordering::SeqCst);
            denied(w)
        };
        for _ in 0..5 {
            drain(&f, &write, &clock);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "attempted exactly once");
        assert_eq!(row(&f, a).attempts, 1);

        // Past the first backoff the row is due again.
        clock.advance(backoff(1));
        drain(&f, &write, &clock);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(row(&f, a).attempts, 2);
        // ...and the second backoff is longer.
        clock.advance(backoff(1));
        drain(&f, &write, &clock);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "backoff doubled");
    }

    #[test]
    fn a_handle_pass_before_the_due_time_drains_nothing() {
        let f = Fix::new("outbox-pass");
        let a = f.insert_image(1, "a.cr3");
        save(&f, a);
        let clock = Arc::new(FakeClock::new());
        let handle = OutboxHandle::with_clock(f.db.clone(), clock.clone());
        let calls = AtomicUsize::new(0);
        let write = |w: &SidecarWrite| {
            calls.fetch_add(1, Ordering::SeqCst);
            denied(w)
        };
        // The fresh intent is due: first pass attempts it, then asks to wait out the backoff.
        let wait = handle.pass(&f.gate, &write, &never);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(wait <= POLL && wait > Duration::ZERO);
        for _ in 0..3 {
            handle.notify();
            handle.pass(&f.gate, &write, &never);
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "wakes before due are inert"
        );
        clock.advance(backoff(1));
        handle.pass(&f.gate, &write, &never);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn remounting_a_source_writes_the_parked_intent_without_a_new_save() {
        let f = Fix::new("outbox-remount");
        let a = f.insert_image(1, "a.cr3");
        save(&f, a);
        set_status(&f, a, "missing");
        let clock = FakeClock::new();
        let report = drain(&f, &production_write, &clock);
        assert_eq!((report.mirrored, report.kept), (0, 1), "{report:?}");
        assert!(f.sidecar("a.cr3").is_none());

        // The card comes back: status flips to present and the schedule is reset (watcher /
        // reconcile / relink do exactly this). No clock movement, no new edit.
        set_status(&f, a, "present");
        reset_outbox_schedule(&f.db.lock().unwrap().conn, Some(a)).unwrap();
        let report = drain(&f, &production_write, &clock);
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert!(f.sidecar("a.cr3").is_some());
        assert!(owed(&f).is_empty());
    }

    #[test]
    fn stop_between_items_leaves_the_rest_queued() {
        let f = Fix::new("outbox-stop");
        let ids: Vec<i64> = (0..5u8)
            .map(|i| f.insert_image(i, &format!("s{i}.cr3")))
            .collect();
        for &id in &ids {
            save(&f, id);
        }
        let written = AtomicUsize::new(0);
        let write = |w: &SidecarWrite| {
            written.fetch_add(1, Ordering::SeqCst);
            production_write(w)
        };
        let stop = || written.load(Ordering::SeqCst) >= 2;
        let report = drain_once(&f.gate, &f.db, &write, &FakeClock::new(), &stop).unwrap();
        assert!(report.interrupted, "{report:?}");
        assert_eq!(report.mirrored, 2);
        assert_eq!(owed(&f).len(), 3, "unvisited rows stay queued");
        let mirrored = ids
            .iter()
            .filter(|&&id| {
                mirrored_revision(&f.db.lock().unwrap().conn, id).unwrap() == Some((1, 1))
            })
            .count();
        assert_eq!(mirrored, 2, "only written rows are marked mirrored");
        assert_eq!(
            (0..5u8)
                .filter(|i| f.sidecar(&format!("s{i}.cr3")).is_some())
                .count(),
            2
        );
    }

    #[test]
    fn a_superseding_save_is_due_immediately_and_not_completed_by_the_old_write() {
        let f = Fix::new("outbox-supersede");
        let a = f.insert_image(1, "a.cr3");
        save(&f, a);
        let clock = FakeClock::new();
        drain(&f, &denied, &clock);
        let backed_off = row(&f, a);
        assert!(backed_off.next_attempt_at > clock.now_ms());
        assert_eq!(backed_off.attempts, 1);

        // A new save supersedes the backed-off intent: due now, attempts back to zero.
        let revision = save(&f, a);
        let fresh = row(&f, a);
        assert_eq!(fresh.edit_revision, revision);
        assert_eq!((fresh.next_attempt_at, fresh.attempts), (0, 0));

        // A write of the OLD revision that lands after a newer save cannot settle the new intent:
        // the in-fence re-read sees different params (Retry) and the deferral is revision-guarded.
        let racing_save = |w: &SidecarWrite| {
            save(&f, a);
            production_write(w)
        };
        drain(&f, &racing_save, &clock);
        let after = row(&f, a);
        assert_eq!(
            after.edit_revision,
            revision + 1,
            "the newer intent survives"
        );
        assert_eq!(after.next_attempt_at, 0, "and is still due now");
        let report = drain(&f, &production_write, &clock);
        assert_eq!(report.mirrored, 1, "{report:?}");
        assert!(no_temp_files(&f));
    }
}
