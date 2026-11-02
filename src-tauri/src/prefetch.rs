//! Predictive preview caching.
//!
//! Stepping through a collection (filmstrip next/next/next) must feel instant. The expensive step
//! per image is the RAW half-res decode (seconds); the GPU upload from a decoded buffer is ~100 ms.
//! So we keep a byte-capped LRU of decoded half-res `LinearImage`s on the CPU:
//!
//! - the interactive preview decode stores its result here (going BACK is instant), and
//! - `develop_prefetch` hands the frontend-announced neighbors to ONE background worker
//!   (going FORWARD is instant). Prefetch is CPU-only — it never touches the GPU queue, so it
//!   cannot contend with interactive renders.
//!
//! **One speculative decode at a time, by construction.** A thread per request used to mean that
//! `next` pressed five times quickly left five decodes running: the generation check aborts a stale
//! set only *between* files, never mid-decode, and `claim_decode` dedups the same image, not
//! different ones. On an 8 GB unified-memory Mac that is several 70–140 MB working sets competing
//! with the foreground decode the user is actually waiting for. A newer request now replaces the
//! queued set instead of adding to it.

use crate::state::AppState;
use core_raw::LinearImage;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use tauri::{AppHandle, Manager};

/// Default LRU byte cap. A half-res 24–33 MP preview is ~70–140 MB, so this holds roughly 3–4 of
/// them — the frontend only ever announces three neighbours (`next`, `prev`, `next+2`), and on an
/// 8 GB machine this cache competes with Metal textures, ONNX models and the WebView for the same
/// physical RAM. Override with `DARKROOM_PREVIEW_LRU_MB` / `DARKROOM_PREVIEW_LRU_ENTRIES` to
/// measure a different budget without a rebuild.
const DEFAULT_LRU_MAX_MB: usize = 384;
/// Entry cap (bounds tiny-image pathologies; bytes are the real constraint).
const DEFAULT_LRU_MAX_ENTRIES: usize = 5;

fn lru_caps() -> (usize, usize) {
    static CAPS: OnceLock<(usize, usize)> = OnceLock::new();
    *CAPS.get_or_init(|| {
        let parse = |key: &str, default: usize| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default)
        };
        let mb = parse("DARKROOM_PREVIEW_LRU_MB", DEFAULT_LRU_MAX_MB);
        let entries = parse("DARKROOM_PREVIEW_LRU_ENTRIES", DEFAULT_LRU_MAX_ENTRIES);
        tracing::debug!(mb, entries, "preview LRU budget");
        (mb * 1024 * 1024, entries)
    })
}

/// Most-recently-used at the BACK. Small (a handful of entries) — linear scans are fine.
#[derive(Default)]
pub struct PreviewLru {
    entries: VecDeque<(i64, Arc<LinearImage>)>,
    bytes: usize,
}

fn image_bytes(img: &LinearImage) -> usize {
    img.data.len() * std::mem::size_of::<f32>()
}

impl PreviewLru {
    /// Fetch + promote to most-recently-used.
    pub fn get(&mut self, id: i64) -> Option<Arc<LinearImage>> {
        let pos = self.entries.iter().position(|(k, _)| *k == id)?;
        let entry = self.entries.remove(pos).expect("position just found");
        let img = entry.1.clone();
        self.entries.push_back(entry);
        Some(img)
    }

    pub fn contains(&self, id: i64) -> bool {
        self.entries.iter().any(|(k, _)| *k == id)
    }

    /// Insert (replacing any same-id entry), then evict least-recently-used past the caps.
    pub fn insert(&mut self, id: i64, img: Arc<LinearImage>) {
        let (max_bytes, max_entries) = lru_caps();
        self.insert_with_caps(id, img, max_bytes, max_entries);
    }

    fn insert_with_caps(
        &mut self,
        id: i64,
        img: Arc<LinearImage>,
        max_bytes: usize,
        max_entries: usize,
    ) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == id) {
            let (_, old) = self.entries.remove(pos).expect("position just found");
            self.bytes -= image_bytes(&old);
        }
        if image_bytes(&img) > max_bytes {
            return;
        }
        self.bytes += image_bytes(&img);
        self.entries.push_back((id, img));
        while self.entries.len() > max_entries || self.bytes > max_bytes {
            if let Some((evicted, old)) = self.entries.pop_front() {
                self.bytes -= image_bytes(&old);
                tracing::trace!(image_id = evicted, bytes = self.bytes, "preview LRU evict");
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

// ---------- bounded speculative decode ----------

#[derive(Default)]
struct QueueState {
    /// The newest requested neighbour set, with the generation it was requested at. A newer request
    /// REPLACES this — stale predictions are worthless, and queueing them would be the bug.
    pending: Option<(u64, Vec<i64>)>,
    shutdown: bool,
}

struct Inner {
    state: Mutex<QueueState>,
    cv: Condvar,
}

/// Handle to the single speculative-decode worker (stored in `AppState`).
#[derive(Clone)]
pub struct PrefetchQueue {
    inner: Arc<Inner>,
}

impl Default for PrefetchQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl PrefetchQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(QueueState::default()),
                cv: Condvar::new(),
            }),
        }
    }

    /// Disposable scheduling state: recover from poisoning rather than killing prefetch for the
    /// rest of the session.
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue a neighbour set, superseding whatever was waiting.
    pub fn request(&self, generation: u64, mut image_ids: Vec<i64>) {
        let mut st = self.lock();
        if st.shutdown {
            return;
        }
        image_ids.truncate(8);
        st.pending = Some((generation, image_ids));
        drop(st);
        self.inner.cv.notify_all();
    }

    pub fn shutdown(&self) {
        let mut st = self.lock();
        st.shutdown = true;
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Block until there is a set to work on. `None` on shutdown.
    fn next_request(&self) -> Option<(u64, Vec<i64>)> {
        let mut st = self.lock();
        loop {
            if st.shutdown {
                return None;
            }
            if let Some(req) = st.pending.take() {
                return Some(req);
            }
            st = self
                .inner
                .cv
                .wait(st)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Decode `image_id`'s half-res preview into the LRU (no GPU work). Returns `Ok(false)` when it
/// was skipped (already cached / claimed elsewhere and finished / superseded).
fn prefetch_one(st: &AppState, image_id: i64, my_gen: u64) -> Result<bool, String> {
    let stale = || st.prefetch_gen.load(Ordering::SeqCst) != my_gen;
    if stale() {
        return Ok(false);
    }
    {
        let lru = st.preview_linear_lru.lock().map_err(|e| e.to_string())?;
        if lru.contains(image_id) {
            return Ok(false);
        }
    }
    let present = || -> Result<bool, String> {
        Ok(st
            .preview_linear_lru
            .lock()
            .map_err(|e| e.to_string())?
            .contains(image_id))
    };
    let _claim = match crate::commands::claim_decode(st, (image_id, true), present, &stale)? {
        crate::commands::ClaimOutcome::Warm | crate::commands::ClaimOutcome::Superseded => {
            return Ok(false)
        }
        crate::commands::ClaimOutcome::Decode(claim) => claim,
    };
    let sequence = st.publications.sequence();
    let row = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?
    };
    let key = core_runtime::publication::SourceKey {
        path: row.path,
        hash: row.content_hash,
    };
    let token = match st.publications.capture_at(
        core_model::identity::ImageId(image_id),
        key.clone(),
        sequence,
    ) {
        Ok(token) => token,
        Err(_) => return Ok(false),
    };
    let src = core_raw::source_from_path(Path::new(&key.path)).map_err(|e| e.to_string())?;
    let lin = core_raw::develop_linear_preview(&src).map_err(|e| e.to_string())?;
    let published = st
        .publications
        .with_current(&token, || -> Result<bool, String> {
            let current = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                let row =
                    core_library::image_by_id(&db.conn, image_id).map_err(|e| e.to_string())?;
                let present = db
                    .conn
                    .query_row(
                        "SELECT status = 'present' FROM images WHERE id = ?1",
                        [image_id],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false);
                present && row.is_some_and(|r| r.path == key.path && r.content_hash == key.hash)
            };
            if !current || stale() {
                return Ok(false);
            }
            st.preview_linear_lru
                .lock()
                .map_err(|e| e.to_string())?
                .insert(image_id, Arc::new(lin));
            Ok(true)
        });
    published.unwrap_or(Ok(false))
}

/// Spawn the single speculative-decode worker. One thread is the point: it caps concurrent
/// speculative RAW decodes at one, so predictions can never crowd out the decode the user is
/// waiting for.
pub fn spawn_worker(app: AppHandle) {
    let queue = {
        let st = app.state::<AppState>();
        st.prefetch_queue.clone()
    };
    let worker_app = app.clone();
    if let Err(error) = crate::jobs::worker(&app, "prefetch", move || {
        while let Some((my_gen, ids)) = queue.next_request() {
            for id in ids {
                let st = worker_app.state::<AppState>();
                if st.prefetch_gen.load(Ordering::SeqCst) != my_gen {
                    break;
                }
                let task_app = worker_app.clone();
                let result = st.jobs.submit(
                    crate::jobs::spec(st.inner(), core_app::jobs::JobClass::Prefetch, false),
                    move |ctx| {
                        ctx.checkpoint()?;
                        let st = task_app.state::<AppState>();
                        let _ = prefetch_one(st.inner(), id, my_gen);
                        Ok(())
                    },
                );
                match result {
                    Ok(handle) => {
                        if let Err(error) = handle.wait() {
                            tracing::debug!(?error, "prefetch job failed");
                        }
                    }
                    Err(error) => tracing::warn!(?error, "prefetch job rejected"),
                }
            }
        }
        Ok(())
    }) {
        tracing::error!(%error, "prefetch worker failed to start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny(id: i64) -> Arc<LinearImage> {
        Arc::new(LinearImage {
            width: 1,
            height: 1,
            data: vec![id as f32; 4],
        })
    }

    #[test]
    fn a_newer_request_replaces_the_queued_one() {
        let q = PrefetchQueue::new();
        q.request(1, vec![10, 11]);
        q.request(2, vec![20, 21]);
        // Stale predictions are worthless: the worker must never work through the old set first.
        assert_eq!(q.next_request(), Some((2, vec![20, 21])));
    }

    #[test]
    fn requests_are_bounded() {
        let q = PrefetchQueue::new();
        q.request(1, (0..100).collect());
        assert_eq!(q.next_request().unwrap().1.len(), 8);
    }

    #[test]
    fn shutdown_releases_the_worker() {
        let q = PrefetchQueue::new();
        q.shutdown();
        assert_eq!(q.next_request(), None);
    }

    #[test]
    fn the_lru_evicts_by_entry_count() {
        let (_, max_entries) = lru_caps();
        let mut lru = PreviewLru::default();
        for id in 0..(max_entries as i64 + 3) {
            lru.insert(id, tiny(id));
        }
        assert_eq!(lru.entries.len(), max_entries);
        assert!(!lru.contains(0), "the oldest entry must be gone");
        assert!(lru.contains(max_entries as i64 + 2), "the newest is kept");
    }

    #[test]
    fn oversized_image_is_not_retained() {
        let mut lru = PreviewLru::default();
        lru.insert_with_caps(1, tiny(1), 15, 5);
        assert!(!lru.contains(1));
        assert_eq!(lru.bytes, 0);
    }

    #[test]
    fn getting_an_entry_promotes_it() {
        let (_, max_entries) = lru_caps();
        let mut lru = PreviewLru::default();
        for id in 0..max_entries as i64 {
            lru.insert(id, tiny(id));
        }
        assert!(lru.get(0).is_some());
        lru.insert(99, tiny(99));
        assert!(
            lru.contains(0),
            "a just-used entry must survive the next insert"
        );
    }
}
