//! Background per-image sidecar (`<raw>.json`) write queue.
//!
//! Sidecars are the durable record of edit intent — disk, not the catalog, is the source of truth a
//! user can carry between machines. They used to be written inline inside the catalog mutation, i.e.
//! with the app's single SQLite connection locked, which is fine for a one-shot rating but not for
//! develop edits: the persistence coordinator now commits *during* a slider drag, so an inline
//! `fsync`-ish rename would sit on the DB lock dozens of times a second.
//!
//! This queue takes that write off the hot path twice over. Producers only mark an id dirty (a set
//! insert under this queue's own lock — never the DB). A single worker thread coalesces every mark
//! landing inside a `COALESCE` window into ONE file write per image, and does the write with the DB
//! lock released — the snapshot is gathered under a brief lock, the bytes hit disk outside it.
//!
//! Failures keep the intent: an id whose source identity or edit moved, or whose file could not be
//! written (read-only volume, full disk), goes to a `deferred` set with exponential backoff instead
//! of being logged and skipped — a sidecar left behind the catalog has nothing else to bring it up to
//! date, and re-marking it immediately would spin on the same failing write forever. A new producer
//! mark resets that backoff (new intent, not a retry). Only a row that is gone is finished without a
//! write, and only `Written`/`Gone` clear the deferred entry: `flush_blocking` reports an honest
//! non-drain while anything is deferred, and [`SidecarQueue::pending_report`] says what is left and
//! why.
//! Writing must still never fail or roll back the catalog mutation that scheduled it, so the worker
//! keeps logging instead. See [`core_runtime::sidecar::write`].

use crate::state::AppState;
use core_db::Db;
use core_library::SidecarWrite;
use core_runtime::publication::PublicationGate;
use core_runtime::sidecar::{production_write, safe_write_error, write_one, SidecarOutcome};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

mod backoff;
use backoff::Deferred;

/// How long the worker lets marks pile up before writing. Batching many edits of one image into one
/// file write is the whole point of this queue — a slider drag marks the same id hundreds of times
/// and must cost exactly one rename. A flush request or shutdown wakes the worker early.
const MAX_PENDING: usize = 4096;
const PAGE_SIZE: usize = 256;
const COALESCE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct QueueState {
    /// Images whose sidecar is stale. A set, so repeat marks of one id collapse for free.
    dirty: HashSet<i64>,
    /// Failed writes backing off. Kept separate from `dirty` so a permanently failing id (read-only
    /// volume) costs one write per backoff window instead of one per coalesce window forever.
    deferred: HashMap<i64, Deferred>,
    /// Set when the shutdown pass has already taken its one last deferred attempt: without it the
    /// worker would re-attempt its still-failing deferred set forever instead of exiting.
    final_pass: bool,
    /// Ids the worker has taken out of `dirty` but not yet written. `flush_blocking` waits on this
    /// too, otherwise it would return "drained" while a batch is still mid-write.
    in_flight: usize,
    scan: Option<i64>,
    scan_generation: u64,
    /// Set by `flush_blocking`: write everything now, don't wait out the coalesce window.
    flush_requested: bool,
    /// Set by `shutdown`: drain what is pending, then let the worker thread exit.
    shutdown: bool,
    failed: bool,
}

#[derive(Debug)]
enum Batch {
    Dirty(Vec<i64>),
    Scan(i64, u64),
}

/// Snapshot of unsatisfied sidecar intent for quit/updater UI.
#[derive(Debug)]
pub struct SidecarPending {
    pub dirty: usize,
    pub deferred: usize,
    /// Deferred ids whose last attempt hit a filesystem error.
    pub failing: usize,
    pub last_error: Option<String>,
}

struct Inner {
    state: Mutex<QueueState>,
    /// Signals both directions: producers/flushers wake the worker, the worker wakes flushers when
    /// a batch completes.
    cv: Condvar,
}

/// Handle to the background sidecar-write queue (stored in `AppState`, cloned into the worker).
#[derive(Clone)]
pub struct SidecarQueue {
    inner: Arc<Inner>,
    /// Injectable time source so backoff is testable without sleeping.
    clock: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl Default for SidecarQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Logged at most once per launch — a poisoned scheduling lock is worth knowing about, but not worth
/// a line per subsequent edit.
static POISON_LOGGED: Once = Once::new();

impl SidecarQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(QueueState::default()),
                cv: Condvar::new(),
            }),
            clock: Arc::new(Instant::now),
        }
    }

    #[cfg(test)]
    fn with_clock(clock: Arc<dyn Fn() -> Instant + Send + Sync>) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(QueueState::default()),
                cv: Condvar::new(),
            }),
            clock,
        }
    }

    fn now(&self) -> Instant {
        (self.clock)()
    }

    /// Mark an image's sidecar stale. Cheap: takes only the queue's own lock, never the DB, so it is
    /// safe to call from inside a catalog transaction / while the DB mutex is held.
    pub fn mark_dirty(&self, image_id: i64) {
        self.mark_dirty_many(&[image_id]);
    }

    pub fn mark_dirty_many(&self, image_ids: &[i64]) {
        let mut st = self.lock();
        if st.shutdown {
            tracing::warn!("sidecar marks rejected after shutdown");
            return;
        }
        for &id in image_ids {
            if st.dirty.contains(&id) {
                continue;
            }
            // A fresh producer mark is new intent: it drops any failed-write backoff so the edit
            // goes out on the next batch instead of after an hour of exponential delay.
            let was_deferred = st.deferred.remove(&id).is_some();
            if !was_deferred && st.dirty.len() + st.deferred.len() >= MAX_PENDING {
                st.scan = Some(i64::MIN);
                st.scan_generation = st.scan_generation.wrapping_add(1);
                break;
            }
            st.dirty.insert(id);
        }
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Ask the worker to write everything pending now, and block until it has (or `timeout` expires).
    /// Returns whether the queue drained *successfully*: deferred entries (failed writes waiting on
    /// backoff) count as pending, so a drain never implies a failing volume got mirrored.
    /// `pending_report` says what is left and why.
    pub fn flush_blocking(&self, timeout: Duration) -> bool {
        let deadline = self.now() + timeout;
        let mut st = self.lock();
        if Self::is_drained(&st) {
            return true;
        }
        st.flush_requested = true;
        self.inner.cv.notify_all();
        loop {
            if st.failed {
                return false;
            }
            if Self::is_drained(&st) {
                return true;
            }
            if st.shutdown {
                return false;
            }
            let Some(remaining) = deadline.checked_duration_since(self.now()) else {
                return false;
            };
            let (guard, timed_out) = match self.inner.cv.wait_timeout(st, remaining) {
                Ok((g, r)) => (g, r.timed_out()),
                Err(p) => {
                    Self::log_poison();
                    let (g, r) = p.into_inner();
                    (g, r.timed_out())
                }
            };
            st = guard;
            if timed_out {
                return Self::is_drained(&st);
            }
        }
    }

    /// A genuine drain: nothing queued, nothing deferred behind a backoff, nothing mid-write.
    fn is_drained(st: &QueueState) -> bool {
        st.dirty.is_empty() && st.deferred.is_empty() && st.scan.is_none() && st.in_flight == 0
    }

    /// What is still waiting to be mirrored, for quit/updater UI: a non-zero `deferred` means the
    /// queue is NOT successfully drained, and `last_error` carries why.
    pub fn pending_report(&self) -> SidecarPending {
        let st = self.lock();
        SidecarPending {
            dirty: st.dirty.len(),
            deferred: st.deferred.len(),
            failing: st
                .deferred
                .values()
                .filter(|d| d.last_error.is_some())
                .count(),
            last_error: st
                .deferred
                .values()
                .min_by_key(|d| d.due)
                .and_then(|d| d.last_error.clone()),
        }
    }

    /// Stop the worker after it has drained what is already pending.
    pub fn shutdown(&self) {
        let mut st = self.lock();
        st.shutdown = true;
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Block until there is work, letting further marks accumulate for up to `COALESCE` first.
    /// Returns the whole dirty set (leaving it empty), due deferred retries, a scan page, or `None`
    /// once shutdown has drained (including its single last-attempt pass over `deferred`).
    fn next_batch(&self) -> Option<Batch> {
        let mut st = self.lock();
        loop {
            if st.failed {
                return None;
            }
            if !st.dirty.is_empty() {
                // Dirty set is non-empty: hold it for the coalesce window unless asked to hurry.
                let deadline = self.now() + COALESCE;
                while !st.flush_requested && !st.shutdown {
                    let Some(remaining) = deadline.checked_duration_since(self.now()) else {
                        break;
                    };
                    st = self.wait(st, Some(remaining));
                }
                return Some(self.take_dirty_batch(&mut st));
            }
            if let Some(batch) = Self::take_scan(&mut st) {
                return Some(batch);
            }
            if let Some(batch) = Self::take_deferred(&mut st, self.now()) {
                return Some(batch);
            }
            if st.shutdown {
                return None;
            }
            if st.flush_requested {
                // Nothing to write — the flusher is already satisfied; tell it so and sleep.
                st.flush_requested = false;
                self.inner.cv.notify_all();
            }
            // Sleep until the earliest backoff is due rather than indefinitely — otherwise a
            // deferred retry would only ever wake on a new mark and never actually be retried.
            let timeout = Self::earliest_due_timeout(&st, self.now());
            st = self.wait(st, timeout);
        }
    }

    /// Non-blocking sibling of `next_batch` sharing its take logic: `None` whenever `next_batch`
    /// would have to wait. Test-only.
    #[cfg(test)]
    fn try_next_batch(&self, now: Instant) -> Option<Batch> {
        let mut st = self.lock();
        if st.failed {
            return None;
        }
        if !st.dirty.is_empty() {
            return Some(self.take_dirty_batch(&mut st));
        }
        if let Some(batch) = Self::take_scan(&mut st) {
            return Some(batch);
        }
        Self::take_deferred(&mut st, now)
    }

    fn take_dirty_batch(&self, st: &mut QueueState) -> Batch {
        st.flush_requested = false;
        let batch: Vec<i64> = st.dirty.drain().collect();
        st.in_flight = batch.len();
        Batch::Dirty(batch)
    }

    fn take_scan(st: &mut QueueState) -> Option<Batch> {
        let cursor = st.scan?;
        st.in_flight = 1;
        Some(Batch::Scan(cursor, st.scan_generation))
    }

    fn fail_batch(&self) {
        let mut st = self.lock();
        st.failed = true;
        st.in_flight = 0;
        drop(st);
        self.inner.cv.notify_all();
    }

    fn finish_scan(&self, generation: u64, ids: &[i64]) {
        let mut st = self.lock();
        if st.scan_generation == generation {
            st.scan = if ids.len() < PAGE_SIZE {
                None
            } else {
                ids.last().copied()
            };
        }
        st.in_flight = 0;
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Called by the worker once a batch is fully written: releases anyone in `flush_blocking`.
    fn finish_batch(&self) {
        let mut st = self.lock();
        st.in_flight = 0;
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Poisoned-lock policy: RECOVER, never panic. This mutex guards disposable scheduling state
    /// (a set of ids and two flags) — a panic elsewhere can leave it inconsistent at worst, and the
    /// next mark/batch repairs that, whereas propagating the poison would silently kill sidecar
    /// persistence for the rest of the session.
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.inner.state.lock().unwrap_or_else(|p| {
            Self::log_poison();
            p.into_inner()
        })
    }

    fn wait<'a>(
        &self,
        st: MutexGuard<'a, QueueState>,
        timeout: Option<Duration>,
    ) -> MutexGuard<'a, QueueState> {
        match timeout {
            None => self.inner.cv.wait(st).unwrap_or_else(|p| {
                Self::log_poison();
                p.into_inner()
            }),
            Some(d) => match self.inner.cv.wait_timeout(st, d) {
                Ok((g, _)) => g,
                Err(p) => {
                    Self::log_poison();
                    p.into_inner().0
                }
            },
        }
    }

    fn log_poison() {
        POISON_LOGGED.call_once(|| {
            tracing::warn!("sidecar queue lock was poisoned; recovering (state is disposable)");
        });
    }
}

/// Spawn the single background worker. One thread is enough: sidecars are small JSON files and the
/// queue exists to serialize + coalesce them, not to parallelize them.
pub fn spawn_worker(app: AppHandle) {
    let queue = {
        let st = app.state::<AppState>();
        st.sidecar_queue.clone()
    };
    let worker_app = app.clone();
    if let Err(error) = crate::jobs::worker(&app, "sidecar-queue", move || {
        while let Some(batch) = queue.next_batch() {
            let result = match batch {
                Batch::Dirty(ids) => {
                    let result = write_batch(&worker_app, &ids);
                    if result.is_ok() {
                        queue.finish_batch();
                    }
                    result
                }
                Batch::Scan(cursor, generation) => {
                    let result = (|| -> Result<Vec<i64>, String> {
                        let st = worker_app.state::<AppState>();
                        let db = st.db.lock().map_err(|e| e.to_string())?;
                        let mut stmt = db.conn.prepare("SELECT id FROM images WHERE status = 'present' AND id > ?1 ORDER BY id LIMIT ?2").map_err(|e| e.to_string())?;
                        let rows = stmt
                            .query_map([cursor, PAGE_SIZE as i64], |r| r.get(0))
                            .map_err(|e| e.to_string())?;
                        rows.collect::<Result<Vec<i64>, _>>()
                            .map_err(|e| e.to_string())
                    })();
                    match result {
                        Ok(ids) => {
                            let result = write_batch(&worker_app, &ids);
                            if result.is_ok() {
                                queue.finish_scan(generation, &ids);
                            }
                            result
                        }
                        Err(error) => Err(error),
                    }
                }
            };
            if let Err(error) = result {
                queue.fail_batch();
                return Err(error);
            }
        }
        Ok(())
    }) {
        tracing::error!(%error, "sidecar worker failed to start");
    }
}

/// Write one coalesced batch. Per id: lock the catalog *only* long enough to gather the snapshot,
/// release it, then put the bytes on disk. Nothing short of a poisoned catalog lock ends the batch:
/// an id that could not be written keeps its dirty mark (see `core_runtime::sidecar::SidecarOutcome`).
fn write_batch(app: &AppHandle, ids: &[i64]) -> Result<(), String> {
    let st = app.state::<AppState>();
    write_ids(
        &st.publications,
        &st.db,
        &st.sidecar_queue,
        ids,
        &production_write,
    )
}

/// The batch body, taken apart from `AppHandle` so the outcome -> dirty-set mapping is testable.
/// Scheduling is unchanged: this only decides, per id, whether its mark is finished.
fn write_ids(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    queue: &SidecarQueue,
    ids: &[i64],
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
) -> Result<(), String> {
    let mut settled = Vec::new();
    for &image_id in ids {
        match write_one(gate, db, image_id, write, &|| {})? {
            // `Unavailable` (row not present) is settled here, as `Gone` was before. Only develop
            // edits carry a durable outbox intent that survives this; rating/flag/keyword marks
            // for a missing source are dropped and are not re-applied on remount (pre-existing).
            SidecarOutcome::Written | SidecarOutcome::Gone | SidecarOutcome::Unavailable => {
                settled.push(image_id);
            }
            SidecarOutcome::Retry => queue.defer(image_id, None, queue.now()),
            SidecarOutcome::Failed(error) => {
                // Why keep the intent: a read-only card or a full volume is transient, and a dropped
                // id leaves the sidecar permanently behind the catalog — nothing else re-marks it. So
                // the id goes to `deferred` with exponential backoff (a spinning failing write must
                // not cost one attempt per coalesce window forever) and the batch moves on.
                tracing::warn!(
                    image_id,
                    error = %safe_write_error(&error),
                    "sidecar write failed; keeping the pending write"
                );
                queue.defer(image_id, Some(error), queue.now());
            }
        }
    }
    queue.settle(&settled);
    Ok(())
}

#[cfg(test)]
mod tests;
