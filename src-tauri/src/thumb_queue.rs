//! Background canonical-thumbnail render queue.
//!
//! A single worker thread renders the unified develop pipeline for images that lack a current
//! cached thumbnail, so the library grid / filmstrip / loupe show the SAME result as the Develop
//! editor — only the resolution differs. The camera-embedded JPEG written at index time is a
//! transient placeholder; this queue replaces it with the canonical render (default params for an
//! unedited image, the stored edit otherwise) and emits `thumb:rendered` so the frontend swaps in
//! the new (versioned) `thumb://` URL.
//!
//! Scheduling: one job runs to completion at a time — the GPU `device.poll` is device-wide and
//! cannot be preempted, so a priority queue can only order the *next* job. While a Develop session
//! is open (`set_interactive(true)`) the worker parks between jobs so interactive renders always win
//! the next GPU slot. A two-tier queue renders visible / just-opened images (front, via
//! `prioritize`) before the bulk backfill (back). Uses a transient `PreparedImage` via
//! `render_develop_jpeg`, never the interactive `full_render_cache`.

use crate::commands::{
    decode_develop, enforce_thumb_cap, render_linear_preview, required_ai_coverages,
    PROCESS_VERSION,
};
use crate::state::AppState;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Once};
use tauri::{AppHandle, Emitter, Manager};

/// Canonical thumbnail long-edge + JPEG quality. 1024 covers the grid (512) and filmstrip (256) via
/// in-browser downscale, and is a good first-paint source for the Develop view (upscaled).
const MAX_PENDING: usize = 4096;
const PAGE_SIZE: usize = 256;
const CANONICAL_EDGE: u32 = 1024;
const CANONICAL_QUALITY: u8 = 85;
/// Display-sharp preview JPEG quality (slightly higher than the thumb — it's viewed full-screen).
const PREVIEW_QUALITY: u8 = 88;
/// Run LRU cap enforcement periodically across background and Develop render writes. Directory
/// scans are not free, so allow a small batch of writes between passes.
const EVICT_EVERY: u64 = 24;
static WRITES_SINCE_EVICT: AtomicU64 = AtomicU64::new(0);

/// Account for a successful render-cache write, including foreground edits while backfill is
/// paused. Call without the catalog lock: cap enforcement briefly acquires it to read settings.
pub(crate) fn enforce_thumb_cap_after_write(st: &AppState) {
    if WRITES_SINCE_EVICT
        .fetch_add(1, Ordering::Relaxed)
        .is_multiple_of(EVICT_EVERY)
    {
        enforce_thumb_cap(st);
    }
}

#[derive(Default)]
struct QueueState {
    /// Visible / just-opened images — rendered before the bulk backfill.
    front: VecDeque<i64>,
    /// Bulk backfill of the rest of the library.
    back: VecDeque<i64>,
    /// Membership set so an id is never queued twice.
    queued: HashSet<i64>,
    /// True while a Develop session is open; the worker parks between jobs so it doesn't contend
    /// with interactive renders for the GPU.
    interactive: bool,
    scan: Option<i64>,
    scan_generation: u64,
    shutdown: bool,
}

enum NextJob {
    Render(i64),
    Refill(i64, u64),
}

struct Inner {
    state: Mutex<QueueState>,
    cv: Condvar,
}

/// Handle to the background canonical-thumbnail queue (stored in `AppState`, cloned into the worker).
#[derive(Clone)]
pub struct ThumbQueue {
    inner: Arc<Inner>,
}

impl Default for ThumbQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Logged at most once per launch — a poisoned scheduling lock is worth knowing about, but not worth
/// a line per subsequent enqueue.
static POISON_LOGGED: Once = Once::new();

impl ThumbQueue {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(QueueState::default()),
                cv: Condvar::new(),
            }),
        }
    }

    /// Append ids to the bulk backfill (deduped). Already-cached ids are cheaply skipped by the
    /// worker's pre-check, so re-enqueuing the whole library after an import is idempotent.
    #[cfg(test)]
    pub fn enqueue_bulk(&self, ids: impl IntoIterator<Item = i64>) {
        let mut st = self.lock();
        if st.shutdown {
            return;
        }
        for id in ids {
            if st.queued.contains(&id) {
                continue;
            }
            if st.queued.len() == MAX_PENDING {
                st.scan = Some(i64::MIN);
                st.scan_generation = st.scan_generation.wrapping_add(1);
                break;
            }
            st.queued.insert(id);
            st.back.push_back(id);
        }
        drop(st);
        self.inner.cv.notify_all();
    }

    pub fn prioritize(&self, ids: &[i64]) {
        let mut st = self.lock();
        if st.shutdown {
            return;
        }
        for &id in ids.iter().rev() {
            st.back.retain(|&x| x != id);
            st.front.retain(|&x| x != id);
            if !st.queued.contains(&id) && st.queued.len() == MAX_PENDING {
                if let Some(evicted) = st.back.pop_back().or_else(|| st.front.pop_back()) {
                    st.queued.remove(&evicted);
                    st.scan = Some(i64::MIN);
                    st.scan_generation = st.scan_generation.wrapping_add(1);
                }
            }
            st.queued.insert(id);
            st.front.push_front(id);
        }
        drop(st);
        self.inner.cv.notify_all();
    }

    fn request_scan(&self) {
        let mut st = self.lock();
        if st.shutdown {
            return;
        }
        st.scan = Some(i64::MIN);
        st.scan_generation = st.scan_generation.wrapping_add(1);
        drop(st);
        self.inner.cv.notify_all();
    }

    pub fn shutdown(&self) {
        self.lock().shutdown = true;
        self.inner.cv.notify_all();
    }

    fn refill(&self, generation: u64, ids: Vec<i64>) {
        let mut st = self.lock();
        if st.scan_generation != generation {
            return;
        }
        st.scan = if ids.len() < PAGE_SIZE {
            None
        } else {
            ids.last().copied()
        };
        for id in ids {
            if st.queued.contains(&id) {
                continue;
            }
            if st.queued.len() == MAX_PENDING {
                st.scan = Some(i64::MIN);
                st.scan_generation = st.scan_generation.wrapping_add(1);
                break;
            }
            st.queued.insert(id);
            st.back.push_back(id);
        }
        self.inner.cv.notify_all();
    }

    /// Mark whether a Develop session is open. Clearing it wakes the worker to resume backfill.
    pub fn set_interactive(&self, active: bool) {
        let mut st = self.lock();
        st.interactive = active;
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Block until a renderable job is available and no Develop session is active. `None` on shutdown.
    fn next_job(&self) -> Option<NextJob> {
        let mut st = self.lock();
        loop {
            if st.shutdown {
                return None;
            }
            if !st.interactive {
                if let Some(id) = st.front.pop_front().or_else(|| st.back.pop_front()) {
                    st.queued.remove(&id);
                    return Some(NextJob::Render(id));
                }
                if let Some(cursor) = st.scan {
                    return Some(NextJob::Refill(cursor, st.scan_generation));
                }
            }
            st = self.inner.cv.wait(st).unwrap_or_else(|p| {
                Self::log_poison();
                p.into_inner()
            });
        }
    }

    /// Poisoned-lock policy: RECOVER, never panic. This mutex guards disposable scheduling state
    /// (two id queues, a membership set and two flags) — a panic elsewhere can leave it inconsistent
    /// at worst, and the next enqueue repairs that, whereas propagating the poison would kill the
    /// background thumbnail backfill (and the worker thread) for the rest of the session. Nothing
    /// here is worth taking the app down for: every job is re-derivable from the catalog.
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.inner.state.lock().unwrap_or_else(|p| {
            Self::log_poison();
            p.into_inner()
        })
    }

    fn log_poison() {
        POISON_LOGGED.call_once(|| {
            tracing::warn!("thumb queue lock was poisoned; recovering (state is disposable)");
        });
    }
}

/// Event payload for `thumb:rendered`: a fresh canonical/edited thumbnail landed on disk for `hash`,
/// so the frontend should cache-bust its `thumb://` URL.
#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ThumbRendered {
    image_id: i64,
    hash: String,
}

/// Spawn the single background worker. No-op (logs) when the GPU is unavailable — without it there
/// is nothing to render and the camera-embedded placeholder remains.
pub fn spawn_worker(app: AppHandle) {
    let queue = {
        let st = app.state::<AppState>();
        if st.gpu.is_none() {
            tracing::warn!("canonical thumbnail backfill disabled: no GPU");
            return;
        }
        st.thumb_queue.clone()
    };
    let worker_app = app.clone();
    if let Err(error) = crate::jobs::worker(&app, "thumb-queue", move || {
        while let Some(job) = queue.next_job() {
            match job {
                NextJob::Refill(cursor, generation) => {
                    let st = worker_app.state::<AppState>();
                    let ids = {
                        let db = st.db.lock().map_err(|e| e.to_string())?;
                        let mut stmt = db.conn.prepare("SELECT id FROM images WHERE status = 'present' AND id > ?1 ORDER BY id LIMIT ?2").map_err(|e| e.to_string())?;
                        let rows = stmt
                            .query_map([cursor, PAGE_SIZE as i64], |r| r.get(0))
                            .map_err(|e| e.to_string())?;
                        rows.collect::<Result<Vec<i64>, _>>()
                            .map_err(|e| e.to_string())?
                    };
                    queue.refill(generation, ids);
                }
                NextJob::Render(id) => loop {
                    let st = worker_app.state::<AppState>();
                    if queue.lock().shutdown {
                        break;
                    }
                    let spec =
                        crate::jobs::spec(st.inner(), core_app::jobs::JobClass::Thumbnail, false);
                    let bytes = spec.estimated_bytes;
                    let task_app = worker_app.clone();
                    match st.jobs.submit(spec, move |ctx| {
                        ctx.checkpoint()?;
                        render_one(&task_app, id);
                        Ok(())
                    }) {
                        Ok(handle) => {
                            if let Err(error) = handle.wait() {
                                tracing::warn!(?error, "thumbnail job failed");
                            }
                            break;
                        }
                        Err(core_app::jobs::SubmitError::Capacity) => {
                            if !st.jobs.wait_capacity(bytes) {
                                break;
                            }
                        }
                        Err(error) => {
                            tracing::warn!(?error, "thumbnail job rejected");
                            break;
                        }
                    }
                },
            }
        }
        Ok(())
    }) {
        tracing::error!(%error, "thumbnail worker failed to start");
    }
}

/// Request paged backfill without materializing the catalog.
pub fn enqueue_all(app: &AppHandle) {
    app.state::<AppState>().thumb_queue.request_scan();
}

/// Render an image's missing tiers (small thumb + display-sharp preview) from ONE decode, then notify
/// the UI to cache-bust. The preview tier is only produced once `preview_edge` is set (the frontend
/// picks it from the display on launch); until then just the thumb renders and the preview is deferred
/// (the set-`preview_edge` IPC re-enqueues). Edited images get `_edit`/`_editpv` variants; unedited get
/// the canonical `_dev`/`_pv` tiers.
fn render_one(app: &AppHandle, image_id: i64) {
    let st = app.state::<AppState>();
    let Some(gpu) = st.gpu.as_ref() else { return };

    // Cheap pre-check (no decode): which tiers are missing? Also read the configured preview edge.
    let sequence = st.publications.sequence();
    let (row, pedge, precheck_version) = {
        let Ok(db) = st.db.lock() else { return };
        let Ok(Some(row)) = core_library::image_by_id(&db.conn, image_id) else {
            return;
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
            return;
        }
        let Ok(Some((_, version))) = core_library::thumb_precheck(&db.conn, image_id) else {
            return;
        };
        (
            row,
            core_library::preview_edge(&db.conn).unwrap_or(0),
            version,
        )
    };
    let key = core_runtime::publication::SourceKey {
        hash: row.content_hash,
        path: row.path,
    };
    let Ok(token) = st.publications.capture_at(
        core_model::identity::ImageId(image_id),
        key.clone(),
        sequence,
    ) else {
        return;
    };
    if st.publications.sequence() != sequence {
        return;
    }
    let need_thumb = match precheck_version {
        Some(v) => st
            .thumbs
            .read_edited(&key.hash, PROCESS_VERSION, v)
            .is_err(),
        None => !st.thumbs.has_canonical(&key.hash, PROCESS_VERSION),
    };
    let need_preview = pedge > 0
        && match precheck_version {
            Some(v) => !st
                .thumbs
                .has_edited_preview(&key.hash, PROCESS_VERSION, v, pedge),
            None => !st.thumbs.has_preview(&key.hash, PROCESS_VERSION, pedge),
        };
    if !need_thumb && !need_preview {
        return;
    }

    // One full-res decode feeds both tiers. Trust the decode's edit_version for the cache keys (the
    // edit may have changed since the pre-check; this is self-correcting on the next enqueue).
    let (hash, params, edit_version, lin) = match decode_develop(st.inner(), image_id) {
        Ok(t) => t,
        Err(_e) => {
            tracing::warn!(image_id, "thumbnail decode failed");
            return;
        }
    };
    let coverages = match required_ai_coverages(st.inner(), image_id, &params) {
        Ok(coverages) => coverages,
        Err(_) => {
            tracing::warn!(image_id, "thumbnail AI mask restoration failed");
            return;
        }
    };
    if hash != key.hash {
        return;
    }
    let publish = |jpeg: &[u8], preview: bool| -> bool {
        st.publications
            .with_current(&token, || {
                let current = {
                    let Ok(db) = st.db.lock() else { return false };
                    let Ok(Some(row)) = core_library::image_by_id(&db.conn, image_id) else {
                        return false;
                    };
                    let present: bool = db
                        .conn
                        .query_row(
                            "SELECT status = 'present' FROM images WHERE id = ?1",
                            [image_id],
                            |r| r.get(0),
                        )
                        .unwrap_or(false);
                    let version = core_library::thumb_precheck(&db.conn, image_id)
                        .ok()
                        .flatten();
                    present
                        && row.path == key.path
                        && row.content_hash == key.hash
                        && version.is_some_and(|(_, v)| v == edit_version)
                };
                if !current {
                    return false;
                }
                match (preview, edit_version) {
                    (false, Some(v)) => st
                        .thumbs
                        .write_edited(&hash, PROCESS_VERSION, v, jpeg)
                        .is_ok(),
                    (false, None) => st
                        .thumbs
                        .write_canonical(&hash, PROCESS_VERSION, jpeg)
                        .is_ok(),
                    (true, Some(v)) => st
                        .thumbs
                        .write_edited_preview(&hash, PROCESS_VERSION, v, pedge, jpeg)
                        .is_ok(),
                    (true, None) => st
                        .thumbs
                        .write_preview(&hash, PROCESS_VERSION, pedge, jpeg)
                        .is_ok(),
                }
            })
            .unwrap_or(false)
    };
    let write_thumb = |jpeg: &[u8]| publish(jpeg, false);
    let write_preview = |jpeg: &[u8]| publish(jpeg, true);

    let mut wrote = false;
    if need_preview {
        // Render the preview at the configured edge; derive the thumb by downscaling its RGBA (one
        // GPU render total — exact for default params; imperceptible for edited thumbs at grid size).
        match render_linear_preview(gpu, lin, &params, &coverages, pedge) {
            Ok((rgba, w, h)) => {
                if let Ok(jpeg) = core_pipeline::rgba8_to_jpeg(&rgba, w, h, PREVIEW_QUALITY) {
                    wrote |= write_preview(&jpeg);
                }
                if need_thumb {
                    let (trgba, tw, th) = core_pipeline::resize_rgba8(&rgba, w, h, CANONICAL_EDGE);
                    if let Ok(jpeg) =
                        core_pipeline::rgba8_to_jpeg(&trgba, tw, th, CANONICAL_QUALITY)
                    {
                        wrote |= write_thumb(&jpeg);
                    }
                }
            }
            Err(_e) => tracing::warn!(image_id, "preview render failed"),
        }
    } else if need_thumb {
        // Preview not wanted yet (edge unset) or already present: render just the thumb.
        match render_linear_preview(gpu, lin, &params, &coverages, CANONICAL_EDGE) {
            Ok((rgba, w, h)) => {
                if let Ok(jpeg) = core_pipeline::rgba8_to_jpeg(&rgba, w, h, CANONICAL_QUALITY) {
                    wrote |= write_thumb(&jpeg);
                }
            }
            Err(_e) => tracing::warn!(image_id, "thumbnail render failed"),
        }
    }

    if wrote {
        let _ = st.publications.with_current(&token, || {
            let _ = app.emit("thumb:rendered", ThumbRendered { image_id, hash });
        });
        enforce_thumb_cap_after_write(&st);
    }
}

#[cfg(test)]
mod tests;
