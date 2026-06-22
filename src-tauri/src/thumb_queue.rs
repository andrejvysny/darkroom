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

use crate::commands::{decode_develop, enforce_thumb_cap, render_linear_cropped, PROCESS_VERSION};
use crate::state::AppState;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{AppHandle, Emitter, Manager};

/// Canonical thumbnail long-edge + JPEG quality. 1024 covers the grid (512) and filmstrip (256) via
/// in-browser downscale, and is a good first-paint source for the Develop view (upscaled).
const CANONICAL_EDGE: u32 = 1024;
const CANONICAL_QUALITY: u8 = 85;
/// Display-sharp preview JPEG quality (slightly higher than the thumb — it's viewed full-screen).
const PREVIEW_QUALITY: u8 = 88;
/// Run the LRU cap-enforcement every N tier-writes during backfill (read_dir is not free; this bounds
/// the evictable-preview disk without statting the cache on every single write).
const EVICT_EVERY: u64 = 24;
static WRITES_SINCE_EVICT: AtomicU64 = AtomicU64::new(0);

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
    shutdown: bool,
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
    pub fn enqueue_bulk(&self, ids: impl IntoIterator<Item = i64>) {
        let mut st = self.inner.state.lock().unwrap();
        let mut added = false;
        for id in ids {
            if st.queued.insert(id) {
                st.back.push_back(id);
                added = true;
            }
        }
        drop(st);
        if added {
            self.inner.cv.notify_all();
        }
    }

    /// Promote ids to the FRONT (visible range / just-opened) so they render next, preserving the
    /// caller's order. Already-queued ids are moved up from the bulk tier.
    pub fn prioritize(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let mut st = self.inner.state.lock().unwrap();
        // Reverse so the caller's first id ends up frontmost after the push_fronts.
        for &id in ids.iter().rev() {
            st.back.retain(|&x| x != id);
            st.front.retain(|&x| x != id);
            st.queued.insert(id);
            st.front.push_front(id);
        }
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Mark whether a Develop session is open. Clearing it wakes the worker to resume backfill.
    pub fn set_interactive(&self, active: bool) {
        let mut st = self.inner.state.lock().unwrap();
        st.interactive = active;
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Block until a renderable job is available and no Develop session is active. `None` on shutdown.
    fn next_job(&self) -> Option<i64> {
        let mut st = self.inner.state.lock().unwrap();
        loop {
            if st.shutdown {
                return None;
            }
            if !st.interactive {
                if let Some(id) = st.front.pop_front().or_else(|| st.back.pop_front()) {
                    st.queued.remove(&id);
                    return Some(id);
                }
            }
            st = self.inner.cv.wait(st).unwrap();
        }
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
            eprintln!("[thumb_queue] no GPU — canonical thumbnail backfill disabled");
            return;
        }
        st.thumb_queue.clone()
    };
    let _ = std::thread::Builder::new()
        .name("thumb-queue".into())
        .spawn(move || {
            while let Some(id) = queue.next_job() {
                render_one(&app, id);
            }
        });
}

/// Enqueue every present image for canonical backfill (startup + after each import). Idempotent.
pub fn enqueue_all(app: &AppHandle) {
    let st = app.state::<AppState>();
    let ids = {
        let Ok(db) = st.db.lock() else { return };
        core_library::present_image_ids(&db.conn).unwrap_or_default()
    };
    st.thumb_queue.enqueue_bulk(ids);
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
    let (pedge, need_thumb, need_preview) = {
        let Ok(db) = st.db.lock() else { return };
        let hash = match core_library::image_by_id(&db.conn, image_id) {
            Ok(Some(img)) => img.content_hash,
            _ => return,
        };
        let edit_version = core_library::get_edit_with_version(&db.conn, image_id)
            .ok()
            .flatten()
            .map(|(_, v)| v);
        let pedge = core_library::preview_edge(&db.conn).unwrap_or(0);
        let need_thumb = match edit_version {
            Some(v) => st.thumbs.read_edited(&hash, v).is_err(),
            None => !st.thumbs.has_canonical(&hash, PROCESS_VERSION),
        };
        let need_preview = pedge > 0
            && match edit_version {
                Some(v) => !st.thumbs.has_edited_preview(&hash, v, pedge),
                None => !st.thumbs.has_preview(&hash, PROCESS_VERSION, pedge),
            };
        (pedge, need_thumb, need_preview)
    };
    if !need_thumb && !need_preview {
        return;
    }

    // One full-res decode feeds both tiers. Trust the decode's edit_version for the cache keys (the
    // edit may have changed since the pre-check; this is self-correcting on the next enqueue).
    let (hash, params, edit_version, lin) = match decode_develop(st.inner(), image_id) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("[thumb_queue] decode image {image_id} failed: {e}");
            return;
        }
    };
    let write_thumb = |jpeg: &[u8]| -> bool {
        match edit_version {
            Some(v) => st.thumbs.write_edited(&hash, v, jpeg).is_ok(),
            None => st
                .thumbs
                .write_canonical(&hash, PROCESS_VERSION, jpeg)
                .is_ok(),
        }
    };
    let write_preview = |jpeg: &[u8]| -> bool {
        match edit_version {
            Some(v) => st
                .thumbs
                .write_edited_preview(&hash, v, pedge, jpeg)
                .is_ok(),
            None => st
                .thumbs
                .write_preview(&hash, PROCESS_VERSION, pedge, jpeg)
                .is_ok(),
        }
    };

    let mut wrote = false;
    if need_preview {
        // Render the preview at the configured edge; derive the thumb by downscaling its RGBA (one
        // GPU render total — exact for default params; imperceptible for edited thumbs at grid size).
        let plin = lin.downscale_into_hq(pedge);
        match render_linear_cropped(gpu, &plin, &params) {
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
            Err(e) => eprintln!("[thumb_queue] preview render image {image_id} failed: {e}"),
        }
    } else if need_thumb {
        // Preview not wanted yet (edge unset) or already present: render just the thumb.
        let tlin = lin.downscale_into_hq(CANONICAL_EDGE);
        match render_linear_cropped(gpu, &tlin, &params) {
            Ok((rgba, w, h)) => {
                if let Ok(jpeg) = core_pipeline::rgba8_to_jpeg(&rgba, w, h, CANONICAL_QUALITY) {
                    wrote |= write_thumb(&jpeg);
                }
            }
            Err(e) => eprintln!("[thumb_queue] thumb render image {image_id} failed: {e}"),
        }
    }

    if wrote {
        let _ = app.emit("thumb:rendered", ThumbRendered { image_id, hash });
        // Bound the evictable (preview) cache off the write path — `enforce_thumb_cap` only ran on
        // import before, so without this the cap would never apply during loupe/backfill rendering.
        if WRITES_SINCE_EVICT
            .fetch_add(1, Ordering::Relaxed)
            .is_multiple_of(EVICT_EVERY)
        {
            enforce_thumb_cap(&st);
        }
    }
}
