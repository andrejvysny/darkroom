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
const COALESCE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct QueueState {
    /// Images whose sidecar is stale. A set, so repeat marks of one id collapse for free.
    dirty: HashSet<i64>,
    /// Ids the worker has taken out of `dirty` but not yet written. `flush_blocking` waits on this
    /// too, otherwise it would return "drained" while a batch is still mid-write.
    in_flight: usize,
    /// Set by `flush_blocking`: write everything now, don't wait out the coalesce window.
    flush_requested: bool,
    /// Set by `shutdown`: drain what is pending, then let the worker thread exit.
    shutdown: bool,
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
        let mut st = self.lock();
        let was_empty = st.dirty.is_empty();
        st.dirty.insert(image_id);
        drop(st);
        // Only the empty → non-empty transition needs a wakeup: it starts the coalesce window. Later
        // marks inside that window deliberately stay silent — they are what we're batching.
        if was_empty {
            self.inner.cv.notify_all();
        }
    }

    pub fn mark_dirty_many(&self, image_ids: &[i64]) {
        if image_ids.is_empty() {
            return;
        }
        let mut st = self.lock();
        let was_empty = st.dirty.is_empty();
        st.dirty.extend(image_ids.iter().copied());
        drop(st);
        if was_empty {
            self.inner.cv.notify_all();
        }
    }

    /// Ask the worker to write everything pending now, and block until it has (or `timeout` expires).
    /// Returns whether the queue drained. Used on app exit / before anything that reads sidecars back.
    pub fn flush_blocking(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut st = self.lock();
        if st.dirty.is_empty() && st.in_flight == 0 {
            return true;
        }
        st.flush_requested = true;
        self.inner.cv.notify_all();
        loop {
            // A genuine drain: nothing queued AND nothing mid-write.
            if st.dirty.is_empty() && st.in_flight == 0 {
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
                return st.dirty.is_empty() && st.in_flight == 0;
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
    fn next_batch(&self) -> Option<Vec<i64>> {
        let mut st = self.lock();
        loop {
            if st.dirty.is_empty() {
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
            return Some(batch);
        }
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
    let _ = std::thread::Builder::new()
        .name("sidecar-queue".into())
        .spawn(move || {
            while let Some(batch) = queue.next_batch() {
                write_batch(&app, &batch);
                queue.finish_batch();
            }
        });
}

/// Write one coalesced batch. Per id: lock the catalog *only* long enough to gather the snapshot,
/// release it, then put the bytes on disk.
fn write_batch(app: &AppHandle, ids: &[i64]) {
    let st = app.state::<AppState>();
    for (i, &image_id) in ids.iter().enumerate() {
        // The DB guard lives in this block and nowhere else. `sidecar_snapshot` is the only step that
        // needs the connection; `write_snapshot` below does pure filesystem work and MUST NOT run
        // with the catalog locked — that is the entire reason this queue exists.
        let snapshot = {
            let Ok(db) = st.db.lock() else {
                // These ids were already drained out of `dirty`; put the untouched remainder back so
                // a later batch still catches up instead of losing them for the session.
                tracing::warn!("sidecar batch skipped: catalog lock poisoned");
                st.sidecar_queue.mark_dirty_many(&ids[i..]);
                return;
            };
            match core_library::sidecar_snapshot(&db.conn, image_id) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(image_id, error = %crate::logging::safe_error(&e), "sidecar snapshot failed");
                    continue;
                }
            }
        };
        // DB lock released here.
        let Some(w) = snapshot else {
            // Row is gone (deleted between the mark and now) — nothing to write.
            continue;
        };
        if let Err(e) = core_library::write_snapshot(&w) {
            tracing::warn!(image_id, error = %crate::logging::safe_error(&e), "sidecar write failed");
        }
    }
}
