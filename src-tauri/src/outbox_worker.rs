//! Drains the durable sidecar-mirror intent queue (`sidecar_outbox`, migration 026).
//!
//! [`crate::sidecar_queue`] answers "this image's sidecar is stale *right now*": its dirty set is
//! in memory, so a crash between the catalog commit and the file write loses the intent and the
//! sidecar stays behind the catalog forever. `core_library::save_edit_with_intent` commits the
//! intent in the same transaction as the edit, so the knowledge survives; this worker is the half
//! that acts on it — one thread that takes the catalog lock briefly to read pending intents,
//! performs each fenced write with the lock released ([`crate::sidecar_write::write_one`]), and
//! reports the outcome back to the catalog ([`core_library::complete_outbox`] /
//! [`core_library::fail_outbox`]).
//!
//! Two properties the rest of the quit/save story depends on:
//! - **Restart recovery.** The first pass after startup drains whatever the previous run left, so
//!   a card that was unplugged mid-write gets mirrored on the next launch without the user doing
//!   anything. Nothing here keeps state a restart would lose.
//! - **Backpressure-free.** A pending intent is *not* an error and never blocks quit: the row is
//!   still owed a write, and the next launch will perform it. [`OutboxHandle::pending_report`] only
//!   informs the log and the UI — `failing` (an attempt that hit the filesystem) is what is worth
//!   surfacing.
//!
//! Retry pacing is derived from the durable `attempts` column, so it survives a restart too: the
//! worker sleeps until the earliest backoff is due and parks indefinitely when the queue is empty.

use crate::sidecar_write::{production_write, safe_write_error, write_one, SidecarOutcome};
use crate::state::AppState;
use core_db::Db;
use core_library::{complete_outbox, fail_outbox, pending_outbox, SidecarWrite};
use core_runtime::publication::PublicationGate;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Intents read per pass. One image can only have one row, so this bounds how long a pass can hold
/// the catalog lock open for its (brief) reads.
const PAGE: usize = 64;
/// Same pacing as the coalescing sidecar queue: a failing card is retried often enough to catch a
/// transient condition (remount, space freed) and rarely enough not to spin on it.
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// The worker parks on a condvar, so the only thing that wakes it for sure is a signal it owns.
/// Poll this often at most so a shutdown that races a park cannot strand the thread (and with it
/// `jobs::shutdown`, which joins every managed worker).
const POLL: Duration = Duration::from_secs(1);

/// Exponential backoff for the `n`-th failed attempt of one intent.
fn backoff(attempts: i64) -> Duration {
    let base = BACKOFF_BASE.as_millis() as u64;
    let shift = u32::try_from(attempts.max(1) - 1).unwrap_or(u32::MAX);
    Duration::from_millis(base.saturating_mul(2u64.saturating_pow(shift))).min(BACKOFF_CAP)
}

/// What one drain pass did. Returned by [`drain_once`] so the worker's pacing decision (sleep until
/// the earliest backoff, or park) and the log line are both derived from observed work.
#[derive(Debug, Default)]
pub(crate) struct DrainReport {
    /// Intents taken off the queue and attempted.
    pub attempted: usize,
    /// Intents whose write succeeded and whose `mirrored_revision` now matches.
    pub mirrored: usize,
    /// Intents dropped without a write: the row or its source moved out from under us.
    pub dropped: usize,
    /// Intents kept for another pass (identity churn, or a failed write).
    pub kept: usize,
    /// When the earliest kept intent may be attempted again (anchored to the pass's `now`, so a
    /// slow pass does not silently shorten a backoff). `None` when nothing was kept.
    pub next_due: Option<Instant>,
    /// Path-masked summary of the most recent write failure seen this pass.
    pub last_error: Option<String>,
}

/// One pass over the queue: read what is owed, write it, report the verdict. Catalog-only reads are
/// brief — the fenced snapshot and the bytes-on-disk happen inside `write_one`, which releases the
/// catalog guard before touching the filesystem.
///
/// `write` is the seam that keeps this testable: production passes
/// [`crate::sidecar_write::production_write`], tests pass a closure that fails on purpose.
/// `Err` is reserved for a catalog that cannot be locked at all; every per-image problem is a
/// counted outcome, never a reason to stop draining.
pub(crate) fn drain_once(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    now: Instant,
) -> Result<DrainReport, String> {
    let items = {
        let db = db.lock().map_err(|_| "catalog lock poisoned")?;
        pending_outbox(&db.conn, PAGE).map_err(|e| safe_write_error(&e.to_string()))?
    };
    let mut report = DrainReport::default();
    for item in items {
        report.attempted += 1;
        match write_one(gate, db, item.image_id, write, &|| {})? {
            SidecarOutcome::Written => {
                // Any answer but `Completed` is still progress: `Superseded` leaves the newer
                // intent queued (its own write proves itself), `Requeued` retries the relinked row
                // immediately against its current identity, `SourceChanged` keeps the intent for a
                // row that is not present right now, and `Gone` cascaded a row that can never be
                // satisfied. Only the write we actually made is claimed.
                let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
                let outcome = complete_outbox(&catalog.conn, &item)
                    .map_err(|e| safe_write_error(&e.to_string()))?;
                drop(catalog);
                match outcome {
                    core_library::OutboxCompletion::Completed => report.mirrored += 1,
                    // The file moved/was renamed mid-write and the intent was re-pointed at the
                    // row's current identity: the write that just succeeded describes a file that
                    // is no longer this image's, so the new path is owed an attempt NOW — no
                    // backoff. Register the row's current identity in the publication gate first
                    // (under the source guard, like the watcher's commits): without it the retry's
                    // `capture` keeps failing on the stale key and the intent spins on Retry —
                    // the exact unpublishable-relink failure this requeue exists to fix. Lock
                    // order: gate before catalog.
                    core_library::OutboxCompletion::Requeued => {
                        let mut source = gate.source_guard();
                        let row = {
                            let catalog = db.lock().map_err(|_| "catalog lock poisoned")?;
                            core_library::image_by_id(&catalog.conn, item.image_id)
                                .map_err(|e| safe_write_error(&e.to_string()))?
                        };
                        if let Some(row) = row {
                            source.replace_source(
                                core_model::identity::ImageId(item.image_id),
                                core_runtime::publication::SourceKey {
                                    hash: row.content_hash,
                                    path: row.path,
                                },
                            );
                            tracing::debug!(
                                image_id = item.image_id,
                                "sidecar intent re-queued at the row's current identity"
                            );
                        }
                        report.kept += 1;
                        keep(&mut report, now, None);
                    }
                    other => {
                        tracing::debug!(
                            image_id = item.image_id,
                            ?other,
                            "sidecar mirror already settled elsewhere"
                        );
                        report.dropped += 1;
                    }
                }
            }
            // The row is deleted or no longer `present`: its intent went with it (FK cascade), so
            // there is nothing left to report and nothing left to write.
            SidecarOutcome::Gone => {
                tracing::debug!(
                    image_id = item.image_id,
                    "sidecar intent orphaned by a deleted source"
                );
                report.dropped += 1;
            }
            // The catalog moved while we were writing: re-snapshot next pass. No `fail_outbox` —
            // this is not a failed write, and charging an attempt for someone else's mutation would
            // age a healthy intent out of the backoff that is meant for a broken volume.
            SidecarOutcome::Retry => {
                report.kept += 1;
                keep(&mut report, now + backoff(item.attempts), None);
            }
            // A real filesystem failure: keep the intent, record why, back off.
            SidecarOutcome::Failed(error) => {
                let masked = safe_write_error(&error);
                tracing::warn!(image_id = item.image_id, error = %masked, "outbox sidecar write failed; keeping the intent");
                let db = db.lock().map_err(|_| "catalog lock poisoned")?;
                fail_outbox(&db.conn, item.image_id, item.edit_revision, &masked)
                    .map_err(|e| safe_write_error(&e.to_string()))?;
                drop(db);
                report.kept += 1;
                keep(&mut report, now + backoff(item.attempts + 1), Some(masked));
            }
        }
    }
    Ok(report)
}

/// Note one kept intent: it is owed another attempt at `due`, and remember why.
fn keep(report: &mut DrainReport, due: Instant, error: Option<String>) {
    report.next_due = Some(match report.next_due {
        Some(existing) => existing.min(due),
        None => due,
    });
    if error.is_some() {
        report.last_error = error;
    }
}

struct Inner {
    db: Arc<Mutex<Db>>,
    /// Producers (`develop_set_edit`) and shutdown wake the worker here.
    cv: Condvar,
    /// The lock the worker parks on; guards nothing but the wait.
    park: Mutex<()>,
    /// Set by [`OutboxHandle::shutdown`]; the worker then finishes its in-hand pass and exits.
    stopping: AtomicBool,
}

/// Shared handle to the mirroring worker, stored in `AppState`.
#[derive(Clone)]
pub struct OutboxHandle {
    inner: Arc<Inner>,
}

impl OutboxHandle {
    pub fn new(db: Arc<Mutex<Db>>) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                cv: Condvar::new(),
                park: Mutex::new(()),
                stopping: AtomicBool::new(false),
            }),
        }
    }

    /// An intent was just committed; go look. Cheap and non-blocking, and safe to call while the
    /// catalog lock is held (this touches only the worker's own mutex).
    pub fn notify(&self) {
        // The predicate the worker waits on is "the queue is non-empty", which it re-reads from the
        // catalog under its own lock; there is no flag to set first, so a bare wake-up is correct
        // and a lost one is bounded by `POLL`.
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

    /// Ask the worker to finish its in-hand pass and exit. Safe to call more than once, and safe to
    /// never call: the poll interval bounds a missed wake-up.
    pub fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.inner.cv.notify_all();
    }

    fn stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::SeqCst)
    }

    /// Park until notified, the earliest backoff is due, or `POLL` elapses.
    fn wait(&self, timeout: Option<Duration>) {
        let guard = self
            .inner
            .park
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let timeout = timeout.map_or(POLL, |d| d.min(POLL));
        let waited = self.inner.cv.wait_timeout(guard, timeout);
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
        loop {
            let report = {
                let st = worker_app.state::<AppState>();
                drain_once(&st.publications, &st.db, &production_write, Instant::now())
            };
            let next_retry = match report {
                Ok(report) => {
                    if report.attempted > 0 {
                        tracing::info!(
                            attempted = report.attempted,
                            mirrored = report.mirrored,
                            dropped = report.dropped,
                            kept = report.kept,
                            last_error = report.last_error.as_deref().unwrap_or(""),
                            "sidecar outbox drained"
                        );
                    }
                    report
                        .next_due
                        .map(|due| due.saturating_duration_since(Instant::now()))
                }
                // A catalog we could not read: keep the intents (that is the whole point of the
                // table) and try again on the next wake-up rather than ending the worker.
                Err(error) => {
                    tracing::warn!(%error, "sidecar outbox pass failed");
                    Some(POLL)
                }
            };
            // Finish the in-hand pass, then leave. The supervisor's flag catches a quit whose
            // shutdown signal raced our park; `handle.stopping()` is the ordered path.
            if handle.stopping() || worker_app.state::<AppState>().workers.is_stopping() {
                handle.shutdown();
                return Ok(());
            }
            handle.wait(next_retry);
        }
    }) {
        tracing::error!(%error, "sidecar outbox worker failed to start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar_write::tests::Fix;
    use core_library::{mirrored_revision, pending_outbox, save_edit_with_intent};

    const PARAMS: &str = "{\"exposure\":0.5}";

    /// The command's durable half: commit an edit plus its mirror intent.
    fn save(f: &Fix, id: i64) -> i64 {
        let db = f.db.lock().unwrap();
        save_edit_with_intent(&db.conn, id, 7, PARAMS, core_library::now_epoch()).unwrap()
    }

    fn owed(f: &Fix) -> Vec<core_library::OutboxItem> {
        pending_outbox(&f.db.lock().unwrap().conn, 64).unwrap()
    }

    fn denied(_: &SidecarWrite) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    }

    #[test]
    fn a_saved_edit_is_mirrored_and_the_intent_settles() {
        let f = Fix::new("outbox-mirror");
        let a = f.insert_image(1, "a.cr3");
        let revision = save(&f, a);

        let report = drain_once(&f.gate, &f.db, &production_write, Instant::now()).unwrap();
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

        let report = drain_once(&f.gate, &f.db, &denied, Instant::now()).unwrap();
        assert_eq!(report.kept, 1, "{report:?}");
        assert!(
            report.next_due.is_some(),
            "a kept intent is timed, not spun on"
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
    }

    #[test]
    fn an_intent_left_by_the_previous_run_is_written_on_the_next_one() {
        let f = Fix::new("outbox-restart");
        let a = f.insert_image(3, "a.cr3");
        save(&f, a);
        // The previous run: the card was not writable, so the intent stayed queued.
        drain_once(&f.gate, &f.db, &denied, Instant::now()).unwrap();
        assert!(f.sidecar("a.cr3").is_none());

        // A restart loses every in-memory queue; the catalog is all that is left.
        let reopened = f.reopened();
        let report = drain_once(
            &reopened.gate,
            &reopened.db,
            &production_write,
            Instant::now(),
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

        // The file is renamed between the fenced write succeeding and the outcome being reported:
        // `write_one` returns `Written` (its in-fence re-read already passed), and `complete_outbox`
        // finds a present row at a new path. The intent must survive re-pointed at that identity —
        // deleting it would strand the edit, since relink never re-queues — and the pass must owe
        // an immediate retry, not a backoff.
        let mid_write_db = f.db.clone();
        let renamed_mid_write = |_: &SidecarWrite| -> std::io::Result<()> {
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
            Ok(())
        };

        let now = Instant::now();
        let report = drain_once(&f.gate, &f.db, &renamed_mid_write, now).unwrap();
        assert_eq!(report.attempted, 1);
        assert_eq!(report.mirrored, 0, "a moved file's write settles nothing");
        assert_eq!(report.kept, 1, "{report:?}");
        assert_eq!(
            report.next_due,
            Some(now),
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
        let report = drain_once(&f.gate, &f.db, &production_write, Instant::now()).unwrap();
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

        drain_once(&f.gate, &f.db, &denied, Instant::now()).unwrap();
        let (pending, failing, last_error) = handle.pending_report();
        assert_eq!(pending, 2, "a failed write still owes a sidecar");
        assert_eq!(failing, 2, "both attempts hit the filesystem");
        assert!(
            last_error.as_deref().is_some_and(|e| e.contains("denied")),
            "{last_error:?}"
        );

        // The volume comes back: settling removes them from the report.
        drain_once(&f.gate, &f.db, &production_write, Instant::now()).unwrap();
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

        let report = drain_once(&f.gate, &f.db, &production_write, Instant::now()).unwrap();
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
}
