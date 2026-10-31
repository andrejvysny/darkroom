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
//! Failures are best-effort and terminal-per-image: a sidecar that can't be written is logged and
//! skipped. It must never fail or roll back the catalog write that scheduled it.

use crate::state::AppState;
use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

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

enum Batch {
    Dirty(Vec<i64>),
    Scan(i64, u64),
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
        }
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
            if st.dirty.len() == MAX_PENDING {
                st.scan = Some(i64::MIN);
                st.scan_generation = st.scan_generation.wrapping_add(1);
                break;
            }
            st.dirty.insert(id);
        }
        drop(st);
        self.inner.cv.notify_all();
    }

    fn retry_dirty(&self, ids: &[i64]) {
        let mut st = self.lock();
        for &id in ids {
            if st.dirty.contains(&id) {
                continue;
            }
            if st.dirty.len() == MAX_PENDING {
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
    /// Returns whether the queue drained. Used on app exit / before anything that reads sidecars back.
    pub fn flush_blocking(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut st = self.lock();
        if st.dirty.is_empty() && st.scan.is_none() && st.in_flight == 0 {
            return true;
        }
        st.flush_requested = true;
        self.inner.cv.notify_all();
        loop {
            if st.failed {
                return false;
            }
            // A genuine drain: nothing queued AND nothing mid-write.
            if st.dirty.is_empty() && st.scan.is_none() && st.in_flight == 0 {
                return true;
            }
            if st.shutdown {
                return false;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
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
                return st.dirty.is_empty() && st.scan.is_none() && st.in_flight == 0;
            }
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
    /// Returns the whole dirty set (leaving it empty) or `None` once shutdown has drained.
    fn next_batch(&self) -> Option<Batch> {
        let mut st = self.lock();
        loop {
            if st.failed {
                return None;
            }
            if st.dirty.is_empty() {
                if let Some(cursor) = st.scan {
                    st.in_flight = 1;
                    return Some(Batch::Scan(cursor, st.scan_generation));
                }
                if st.shutdown {
                    return None;
                }
                if st.flush_requested {
                    // Nothing to write — the flusher is already satisfied; tell it so and sleep.
                    st.flush_requested = false;
                    self.inner.cv.notify_all();
                }
                st = self.wait(st, None);
                continue;
            }
            // Dirty set is non-empty: hold it for the coalesce window unless asked to hurry.
            let deadline = Instant::now() + COALESCE;
            while !st.flush_requested && !st.shutdown {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                st = self.wait(st, Some(remaining));
            }
            st.flush_requested = false;
            let batch: Vec<i64> = st.dirty.drain().collect();
            st.in_flight = batch.len();
            return Some(Batch::Dirty(batch));
        }
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
/// release it, then put the bytes on disk.
fn write_batch(app: &AppHandle, ids: &[i64]) -> Result<(), String> {
    let st = app.state::<AppState>();
    for &image_id in ids {
        // The DB guard lives in this block and nowhere else. `sidecar_snapshot` is the only step that
        // needs the connection; `write_snapshot` below does pure filesystem work and MUST NOT run
        // with the catalog locked — that is the entire reason this queue exists.
        let sequence = st.publications.sequence();
        let snapshot = {
            let Ok(db) = st.db.lock() else {
                return Err("sidecar catalog lock poisoned".into());
            };
            let Ok(Some(row)) = core_library::image_by_id(&db.conn, image_id) else {
                continue;
            };
            let present: bool = db
                .conn
                .query_row(
                    "SELECT status = 'present' FROM images WHERE id = ?1",
                    [image_id],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !present {
                continue;
            }
            let Ok(Some(snapshot)) = core_library::sidecar_snapshot(&db.conn, image_id) else {
                continue;
            };
            (row, snapshot)
        };
        let (row, w) = snapshot;
        let key = core_runtime::publication::SourceKey {
            hash: row.content_hash,
            path: row.path,
        };
        let Ok(token) = st.publications.capture_at(
            core_model::identity::ImageId(image_id),
            key.clone(),
            sequence,
        ) else {
            tracing::warn!(image_id, "sidecar skipped: source identity stale");
            continue;
        };
        let result = st.publications.with_current(&token, || -> Result<bool, String> {
            let current = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                let Ok(Some(row)) = core_library::image_by_id(&db.conn, image_id) else { return Ok(true) };
                let present: bool = db.conn.query_row(
                    "SELECT status = 'present' FROM images WHERE id = ?1", [image_id], |r| r.get(0),
                ).unwrap_or(false);
                if !present || row.path != key.path || row.content_hash != key.hash { return Ok(true); }
                let Ok(Some(mut latest)) = core_library::sidecar_snapshot(&db.conn, image_id) else { return Ok(true) };
                latest.sidecar.updated_at = w.sidecar.updated_at;
                latest.path == w.path && latest.sidecar == w.sidecar
            };
            if !current { return Ok(false); }
            if let Err(e) = core_library::write_snapshot(&w) {
                tracing::warn!(image_id, error = %crate::logging::safe_error(&e), "sidecar write failed");
            }
            Ok(true)
        });
        match result {
            Ok(Ok(false)) => st.sidecar_queue.retry_dirty(&[image_id]),
            Ok(Err(error)) => return Err(error),
            Err(_) => tracing::warn!(image_id, "sidecar skipped: source identity stale"),
            Ok(Ok(true)) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    #[test]
    fn overflow_keeps_dirty_marks_and_requests_rescan() {
        let q = SidecarQueue::new();
        let ids: Vec<_> = (0..=MAX_PENDING as i64).collect();
        q.mark_dirty_many(&ids);
        q.mark_dirty(0);
        let state = q.lock();
        assert_eq!(state.dirty.len(), MAX_PENDING);
        assert_eq!(state.scan, Some(i64::MIN));
    }

    #[test]
    fn failed_scan_wakes_flush_and_reports_failure() {
        let q = SidecarQueue::new();
        {
            let mut state = q.lock();
            state.scan = Some(i64::MIN);
        }
        assert!(matches!(q.next_batch(), Some(Batch::Scan(_, _))));
        q.fail_batch();
        assert!(!q.flush_blocking(Duration::ZERO));
        assert!(q.next_batch().is_none());
    }

    #[test]
    fn shutdown_drains_dirty_and_rescan() {
        let q = SidecarQueue::new();
        q.mark_dirty(1);
        {
            let mut state = q.lock();
            state.scan = Some(i64::MIN);
        }
        q.shutdown();
        q.mark_dirty(2);
        assert!(matches!(q.next_batch(), Some(Batch::Dirty(ids)) if ids == vec![1]));
        q.finish_batch();
        let Some(Batch::Scan(_, generation)) = q.next_batch() else {
            panic!("expected scan")
        };
        q.finish_scan(generation, &[]);
        assert!(q.next_batch().is_none());
        assert!(q.flush_blocking(Duration::ZERO));
    }
}
