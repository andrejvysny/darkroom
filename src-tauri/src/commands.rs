//! IPC command handlers. Heavy/DB work runs on `spawn_blocking`; state is fetched via the
//! `AppHandle` inside the blocking closure (never held across `.await`).

use crate::state::AppState;
use core_library::{
    CaptionRow, CollectionRow, DetectionRow, FacetRow, FolderRow, ImageFaceRow, ImageRow,
    IndexStats, KeywordRow, PersonFaceRow, PersonRow, PresenceRow, QueryParams, UserLabels,
};
use core_pipeline::{DevelopParams, Histogram};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

// v2 adds local adjustment masks (DevelopParams.masks). v1 rows deserialize with masks: [].
// v3 adds the scene-referred base tone operator (DevelopParams.tone_amount, default 100 = full ACR);
// it replaces the fixed highlight shoulder, so v1/v2 rows re-render with the new tonality.
// v4 fits the base tone curve to the real ACR default (mid-grey 0.18→0.388 ≈65% sRGB, ~+1.3 EV
// brighter) + adds Color-balance-RGB; all prior rows re-render with the matched ACR brightness.
const PROCESS_VERSION: i64 = 4;

/// Delete cached thumbnails for content hashes no longer referenced by any present row. A byte-
/// identical keeper still shares its hash, so presence is re-checked before deleting (lowercase-hex
/// compare against the stored BLOB).
fn gc_orphan_thumbs(
    conn: &core_db::rusqlite::Connection,
    thumbs: &core_library::ThumbCache,
    hashes: &[String],
) {
    use core_db::rusqlite::OptionalExtension;
    for h in hashes {
        let still_present = conn
            .query_row(
                "SELECT 1 FROM images WHERE lower(hex(content_hash)) = ?1 AND status='present' LIMIT 1",
                [h],
                |_| Ok(()),
            )
            .optional();
        if matches!(still_present, Ok(None)) {
            let _ = thumbs.remove_hash(h);
        }
    }
}

/// Evict thumbnails down to the configured cache cap (best-effort). The cap is read under a brief
/// lock, then released before the (slower) filesystem eviction runs.
fn enforce_thumb_cap(st: &AppState) {
    let cap = {
        let Ok(db) = st.db.lock() else { return };
        core_library::thumb_cache_cap(&db.conn).unwrap_or(core_library::DEFAULT_THUMB_CACHE_CAP)
    };
    let _ = st.thumbs.evict_to(cap);
}

#[tauri::command]
pub async fn library_query(app: AppHandle, params: QueryParams) -> Result<Vec<ImageRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::query_images(&db.conn, &params).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_count(app: AppHandle, params: QueryParams) -> Result<i64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::count_images(&db.conn, &params).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_folders(app: AppHandle) -> Result<Vec<FolderRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_folders(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn image_meta(app: AppHandle, id: i64) -> Result<Option<ImageRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Index (or re-index) a folder as a watched root. Emits `import:progress`/`import:done`.
///
/// The DB lock is held only briefly (root upsert + known-path read, then the transactional
/// insert) — the multi-second parallel decode/hash/thumbnail work runs UNLOCKED so concurrent
/// `library_query` calls stay responsive during indexing.
#[tauri::command]
pub async fn library_index_root(app: AppHandle, path: String) -> Result<IndexStats, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();
        index_root_blocking(&app2, &st, &PathBuf::from(&path), true)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Index every supported RAW under `root` into the catalog: upsert the folder, enumerate
/// (recursively), parallel hash+meta+thumbnail, then a single transactional insert. Emits
/// `import:progress` / `import:done`. When `analyze`, kicks off background AI analysis of the
/// newly-added images (only if models are already downloaded). Runs synchronously on the caller's
/// (blocking) thread.
fn index_root_blocking(
    app: &AppHandle,
    st: &AppState,
    root: &Path,
    analyze: bool,
) -> Result<IndexStats, String> {
    // --- brief lock: upsert folder + snapshot known paths ---
    let (folder_id, known) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let fid = core_library::add_root(&db.conn, root).map_err(|e| e.to_string())?;
        let known = core_library::existing_paths(&db.conn).map_err(|e| e.to_string())?;
        (fid, known)
    };

    // --- unlocked: enumerate + parallel process (hash + meta + thumbnail) ---
    let todo: Vec<PathBuf> = core_library::enumerate_raws(root, true)
        .into_iter()
        .filter(|p| !known.contains(&p.display().to_string()))
        .collect();
    let total = todo.len();
    let done = AtomicUsize::new(0);
    let results: Vec<_> = todo
        .par_iter()
        .map(|p| {
            let r = core_library::process_file(p, &st.thumbs, core_library::THUMB_SIZE);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n == total || n.is_multiple_of(4) {
                let _ = app.emit(
                    "import:progress",
                    serde_json::json!({"done": n, "total": total}),
                );
            }
            r
        })
        .collect();

    // --- brief lock: transactional insert ---
    let imported_at = core_library::now_epoch();
    let mut stats = IndexStats {
        scanned: total,
        ..Default::default()
    };
    {
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.transaction().map_err(|e| e.to_string())?;
        for r in &results {
            match r {
                Ok(p) => match core_library::insert_image(&tx, folder_id, imported_at, p)
                    .map_err(|e| e.to_string())?
                {
                    Some(_) => stats.added += 1,
                    None => stats.skipped += 1,
                },
                Err(_) => stats.failed += 1,
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    enforce_thumb_cap(st);
    let _ = app.emit("import:done", &stats);

    // Auto-analyze newly indexed images in the background — but only if the models are already
    // downloaded (never trigger a ~400 MB download implicitly). First-time analysis is explicit.
    if analyze && stats.added > 0 && crate::analysis::models_ready(st) {
        let app3 = app.clone();
        std::thread::spawn(move || {
            let _ = crate::analysis::run_pass(&app3, false);
        });
    }
    Ok(stats)
}

/// Wipe the catalog to a fully empty state. Deletes every DB row (images, folders/watched roots,
/// edits, collections, keywords, analysis, behavioral events…) and clears the thumbnail + warm GPU
/// caches. Files on disk are never touched — only the index/metadata is removed. Nothing is
/// re-scanned: the app is left empty until the user imports again. Returns zeroed stats.
#[tauri::command]
pub async fn database_reset(app: AppHandle) -> Result<IndexStats, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();

        // Wipe all catalog rows (folders included → no watched roots remain).
        {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            db.wipe().map_err(|e| e.to_string())?;
        }
        // Clear derived caches (thumbnails on disk + warm GPU resources + last histogram).
        let _ = st.thumbs.evict_to(0);
        if let Ok(mut slot) = st.full_render_cache.lock() {
            *slot = None;
        }
        if let Ok(mut h) = st.last_histogram.lock() {
            *h = None;
        }

        Ok::<_, String>(IndexStats::default())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Dev convenience: the bundled `library/2026` validation folder, if present on disk.
/// Returns `None` in a packaged app where that path does not exist.
#[tauri::command]
pub fn app_default_library() -> Option<String> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../library/2026");
    p.canonicalize()
        .ok()
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
}

// ---------- Develop ----------

/// Saved develop params for an image (defaults if none).
#[tauri::command]
pub async fn develop_get_edit(app: AppHandle, image_id: i64) -> Result<DevelopParams, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let params = match core_library::get_edit(&db.conn, image_id).map_err(|e| e.to_string())? {
            Some(json) => serde_json::from_str::<DevelopParams>(&json).unwrap_or_else(|e| {
                // Don't silently default — surface the parse failure. The stored blob is left intact
                // on disk; `develop_set_edit` refuses to overwrite an unreadable blob (unless forced
                // by an explicit Reset), so the user's real edit isn't destroyed by the next auto-save.
                eprintln!(
                    "[darkroom] develop params for image {image_id} failed to parse ({e}); \
                     showing defaults but preserving the stored edit"
                );
                DevelopParams::default()
            }),
            None => DevelopParams::default(),
        };
        Ok::<_, String>(params)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Persist develop params (non-destructive; originals are never touched).
#[tauri::command]
pub async fn develop_set_edit(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
    touch_count: Option<i64>,
    // `true` to overwrite even an unreadable stored blob (explicit Reset). Defaults to `false`.
    force: Option<bool>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let json = serde_json::to_string(&params).map_err(|e| e.to_string())?;
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        // Snapshot the prior params for the event before overwriting.
        let before = core_library::get_edit(&db.conn, image_id).map_err(|e| e.to_string())?;
        // Refuse to clobber a stored edit that exists but no longer parses into the current schema
        // (corruption / breaking change). Overwriting it would permanently destroy the user's real
        // adjustments — the non-destructive guarantee. An explicit Reset passes `force=true` to
        // discard it deliberately; a stray slider commit (no force) cannot.
        if !force.unwrap_or(false) {
            if let Some(prev) = &before {
                if serde_json::from_str::<DevelopParams>(prev).is_err() {
                    return Err(format!(
                        "stored edit for image {image_id} is unreadable (schema mismatch or \
                         corruption); Reset to discard it"
                    ));
                }
            }
        }
        core_library::set_edit(
            &db.conn,
            image_id,
            PROCESS_VERSION,
            &json,
            core_library::now_epoch(),
        )
        .map_err(|e| e.to_string())?;
        sync_sidecar(&db.conn, image_id);
        let _ = core_library::append_event(
            &db.conn,
            &crate::events::stamp(
                st.inner(),
                core_library::Event {
                    event_type: "develop.params_commit".into(),
                    image_id: Some(image_id),
                    process_version: Some(PROCESS_VERSION),
                    params_before: before,
                    params_after: Some(json),
                    touch_count,
                    ..Default::default()
                },
            ),
        );
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

const PREVIEW_MAX_EDGE: u32 = 1600;
/// Hard cap on the full-res texture's long edge — keeps it within the GPU max texture dimension
/// (8192 on the wgpu defaults). A no-op for the validated EOS R7 (6960 px).
const FULL_MAX_EDGE: u32 = 8192;

fn profiling() -> bool {
    std::env::var_os("DARKROOM_PROFILE").is_some()
}

/// Demosaic-free instant first paint: the camera's embedded preview JPEG (no GPU, no decode of the
/// sensor data, no cache lock). The frontend shows this within ~tens of ms while `develop_render`
/// produces the color-managed, edit-applied result in the background.
#[tauri::command]
pub async fn develop_preview_jpeg(
    app: AppHandle,
    image_id: i64,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let path = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::image_by_id(&db.conn, image_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "image not found".to_string())?
                .path
        };
        let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
        let thumb = core_raw::thumbnail_jpeg(&src, 2560, 85).map_err(|e| e.to_string())?;
        Ok::<_, String>(tauri::ipc::Response::new(thumb.jpeg))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Library loupe: the unedited capture (camera embedded preview, near full sensor res on CR3) as
/// JPEG bytes. `max_edge == 0` returns native size (capped at 8192 to bound payload/decode); any
/// positive value downscales the long edge to that. No GPU, no develop edits — the frontend loads
/// 2560 for fit-view, then native on zoom. Distinct from `develop_preview_jpeg` (which the Develop
/// view owns) so the two can evolve independently.
#[tauri::command]
pub async fn loupe_jpeg(
    app: AppHandle,
    image_id: i64,
    max_edge: u32,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let path = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::image_by_id(&db.conn, image_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "image not found".to_string())?
                .path
        };
        let edge = if max_edge == 0 { 8192 } else { max_edge };
        // Edited images: show the develop-rendered result so the loupe matches the editor. Unedited
        // images (or no GPU) fall back to the fast embedded preview.
        if let Some(gpu) = st.gpu.as_ref() {
            if let (_, Some((jpeg, _))) =
                render_edit_jpeg(st.inner(), gpu, image_id, edge, 90)?
            {
                return Ok(tauri::ipc::Response::new(jpeg));
            }
        }
        let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
        let thumb = core_raw::thumbnail_jpeg(&src, edge, 90).map_err(|e| e.to_string())?;
        Ok::<_, String>(tauri::ipc::Response::new(thumb.jpeg))
    })
    .await
    .map_err(|e| e.to_string())?
}

const THUMB_EDIT_EDGE: u32 = 1024;
const THUMB_EDIT_QUALITY: u8 = 85;

/// Result of an edited-render attempt: the image's content hash plus, when it has an edit, the
/// rendered JPEG bytes and the edit version (`updated_at`). `None` render → caller falls back to the
/// embedded/base thumbnail.
type EditRender = (String, Option<(Vec<u8>, i64)>);

/// Render an image's stored develop edit to JPEG at `max_edge`. Uses the full demosaic above the
/// preview cap so loupe/zoom stay sharp; the cheaper superpixel decode at/below it.
fn render_edit_jpeg(
    st: &AppState,
    gpu: &crate::state::GpuRender,
    image_id: i64,
    max_edge: u32,
    quality: u8,
) -> Result<EditRender, String> {
    let (path, hash_hex) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let img = core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?;
        (img.path, img.content_hash)
    };
    let edit = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::get_edit_with_version(&db.conn, image_id).map_err(|e| e.to_string())?
    };
    let (params_json, version) = match edit {
        Some(v) => v,
        None => return Ok((hash_hex, None)),
    };
    let params: DevelopParams = serde_json::from_str(&params_json).map_err(|e| e.to_string())?;
    let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
    let lin = if max_edge > PREVIEW_MAX_EDGE {
        core_raw::develop_linear(&src).map_err(|e| e.to_string())?
    } else {
        core_raw::develop_linear_preview(&src).map_err(|e| e.to_string())?
    }
    .downscale_into(max_edge);
    let (w, h) = (lin.width, lin.height);
    let rgba = gpu
        .pipeline
        .render_once(&gpu.ctx, &lin, &params)
        .map_err(|e| e.to_string())?;
    let jpeg = core_pipeline::rgba8_to_jpeg(&rgba, w, h, quality).map_err(|e| e.to_string())?;
    Ok((hash_hex, Some((jpeg, version))))
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditChanged {
    image_id: i64,
    /// Edit version (`updated_at`) for cache-busting previews, or null when the edit was cleared.
    edited_at: Option<i64>,
}

/// Regenerate the edited thumbnail for an image and notify the frontend so the grid/filmstrip swap
/// to the new versioned `thumb://` URL. Called on edit-settle (after persist). Returns the new edit
/// version, or null when the image has no edit (its edited variants are then cleared).
#[tauri::command]
pub async fn develop_regen_thumb(app: AppHandle, image_id: i64) -> Result<Option<i64>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let gpu = st
            .gpu
            .as_ref()
            .ok_or_else(|| "GPU develop unavailable".to_string())?;
        let (hash, render) =
            render_edit_jpeg(st.inner(), gpu, image_id, THUMB_EDIT_EDGE, THUMB_EDIT_QUALITY)?;
        let edited_at = match render {
            Some((jpeg, version)) => {
                st.thumbs
                    .write_edited(&hash, version, &jpeg)
                    .map_err(|e| e.to_string())?;
                Some(version)
            }
            None => {
                let _ = st.thumbs.clear_edited(&hash);
                None
            }
        };
        let _ = app.emit("develop:edit-changed", EditChanged { image_id, edited_at });
        Ok(edited_at)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The histogram of the most recent successful render. A reliable pull-fallback for the
/// fire-and-forget `develop:histogram` event (which can be missed if it fires before the listener
/// is registered). Returns `None` if nothing has rendered yet.
#[tauri::command]
pub async fn develop_get_histogram(app: AppHandle) -> Result<Option<Histogram>, String> {
    let st = app.state::<AppState>();
    let hist = st.last_histogram.lock().map_err(|e| e.to_string())?.clone();
    Ok(hist)
}

/// A crop-local uv viewport window: origin `(ox, oy)` + size `(sx, sy)`, all in [0,1] crop space.
#[derive(serde::Deserialize)]
pub struct ViewRect {
    ox: f32,
    oy: f32,
    sx: f32,
    sy: f32,
}

/// Resolve a frontend mask *index* (its position in `params.masks`) to the dense GPU overlay
/// *layer* the mask pre-pass packs into. The pre-pass only uploads enabled masks, so disabled
/// masks before `idx` shift every later mask down a layer. Returns -1 (no overlay) when `idx` is
/// negative, out of range, the targeted mask is disabled, or its packed layer would exceed the GPU
/// mask cap.
fn packed_overlay_layer(params: &DevelopParams, idx: i32) -> i32 {
    if idx < 0 {
        return -1;
    }
    let i = idx as usize;
    match params.masks.get(i) {
        Some(m) if m.enabled => {
            let packed = params.masks[..i].iter().filter(|m| m.enabled).count();
            if packed >= core_pipeline::MASK_CAP {
                -1
            } else {
                packed as i32
            }
        }
        _ => -1,
    }
}

/// Render a crop-local viewport window of `image_id` at the requested output size, returning raw
/// RGBA8 framed by its dimensions: `[out_w u32 LE][out_h u32 LE][rgba8 (out_w*out_h*4)]`.
///
/// The prepared SOURCE is always full-resolution (decoded once and cached in `full_render_cache`,
/// keyed by `image_id`) so any zoom stays crisp; only the small `out_w × out_h` viewport target is
/// re-rendered per edit (the RapidRAW pattern). The expensive decode runs WITHOUT holding the
/// cache lock. `request_id` is a monotonic supersede token: if a newer request has overtaken this
/// one, the decode is skipped and an empty response is returned. (Instant first-paint is served
/// separately by `develop_preview_jpeg`.)
#[allow(clippy::too_many_arguments)] // fixed by the viewport-render IPC contract
#[tauri::command]
pub async fn develop_render(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
    view: ViewRect,
    out_w: u32,
    out_h: u32,
    overlay_mask_index: i32,
    request_id: u64,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let prof = profiling();
        let st = app.state::<AppState>();
        let gpu = st
            .gpu
            .as_ref()
            .ok_or_else(|| "GPU develop unavailable".to_string())?;

        st.latest_render.fetch_max(request_id, Ordering::SeqCst);
        let superseded = || st.latest_render.load(Ordering::SeqCst) > request_id;

        // Free the large full-res texture as soon as we render a different image.
        {
            let mut slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
            if slot.as_ref().is_some_and(|(id, _)| *id != image_id) {
                *slot = None;
            }
        }

        // --- Full-res source: decode the whole frame at full demosaic, cached in a single slot. ---
        let present = {
            let slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
            slot.as_ref().is_some_and(|(id, _)| *id == image_id)
        };
        if !present {
            // Skip the expensive decode if a newer request already arrived.
            if superseded() {
                return Ok(tauri::ipc::Response::new(Vec::new()));
            }
            let path = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::image_by_id(&db.conn, image_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "image not found".to_string())?
                    .path
            };
            let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
            let t = Instant::now();
            let lin = core_raw::develop_linear(&src)
                .map_err(|e| e.to_string())?
                .downscale_into(FULL_MAX_EDGE);
            if prof {
                eprintln!("[profile] decode(full): {:?}", t.elapsed());
            }
            let prepared = gpu
                .pipeline
                .prepare(&gpu.ctx, &lin)
                .map_err(|e| e.to_string())?;
            *st.full_render_cache.lock().map_err(|e| e.to_string())? = Some((image_id, prepared));
        }

        // Build the viewport descriptor and render the crop-local window into an out_w × out_h
        // target. Lock held across render+readback so the slot can't be swapped mid-render.
        let view = core_pipeline::ViewParams {
            origin: [view.ox, view.oy],
            size: [view.sx, view.sy],
            out_w,
            out_h,
            active: true,
            overlay_layer: packed_overlay_layer(&params, overlay_mask_index),
            overlay_color: [0.85, 0.10, 0.10],
            overlay_strength: 0.5,
        };
        let slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
        let (_, prepared) = slot
            .as_ref()
            .ok_or_else(|| "full-res image evicted before render".to_string())?;
        let t = Instant::now();
        let rgba = gpu
            .pipeline
            .render_view(&gpu.ctx, prepared, &params, &view)
            .map_err(|e| e.to_string())?;
        if prof {
            eprintln!("[profile] gpu render_view+readback: {:?}", t.elapsed());
        }
        drop(slot);

        // Histogram from the rendered buffer: store for pull + emit for push. Skip if a newer render
        // has superseded this one — otherwise a slower earlier render (e.g. a cache hit racing a
        // just-started newer request) could clobber the live histogram with a stale buffer's stats.
        // TODO: whole-crop histogram pass (this is now the visible-viewport histogram).
        if !superseded() {
            let hist = core_pipeline::histogram(&rgba);
            if let Ok(mut last) = st.last_histogram.lock() {
                *last = Some(hist.clone());
            }
            let _ = app.emit("develop:histogram", hist);
        }

        // Frame the raw RGBA8 with its dimensions: [out_w LE][out_h LE][rgba8].
        let mut buf = Vec::with_capacity(8 + rgba.len());
        buf.extend_from_slice(&out_w.to_le_bytes());
        buf.extend_from_slice(&out_h.to_le_bytes());
        buf.extend_from_slice(&rgba);
        Ok::<_, String>(tauri::ipc::Response::new(buf))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Export ----------

/// Export a single image at full resolution through the develop pipeline to `dest`.
/// `format` is "png" or "jpeg". Originals are never modified.
#[tauri::command]
pub async fn export_image(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
    format: String,
    dest: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let gpu = st
            .gpu
            .as_ref()
            .ok_or_else(|| "GPU export unavailable".to_string())?;

        let path = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::image_by_id(&db.conn, image_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "image not found".to_string())?
                .path
        };

        let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
        let lin = core_raw::develop_linear(&src).map_err(|e| e.to_string())?;
        let rgba = gpu
            .pipeline
            .render_once(&gpu.ctx, &lin, &params)
            .map_err(|e| e.to_string())?;

        // Crop to true dimensions: the render letterbox-fits the crop centered into the full frame,
        // so a plain pixel copy of that rect is the exact cropped export (no extra resample).
        let (cx, cy, cw, ch) = params.crop.export_rect(lin.width, lin.height);
        let rgba = if (cw, ch) == (lin.width, lin.height) {
            rgba
        } else {
            core_pipeline::crop_rgba8(&rgba, lin.width, cx, cy, cw, ch)
        };

        let bytes = match format.to_lowercase().as_str() {
            "png" => core_pipeline::rgba8_to_png(&rgba, cw, ch),
            "jpeg" | "jpg" => core_pipeline::rgba8_to_jpeg(&rgba, cw, ch, 92),
            other => return Err(format!("unsupported export format: {other}")),
        }
        .map_err(|e| e.to_string())?;

        std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
        // Export is the strongest edit-quality endorsement — log it (best-effort, separate lock).
        crate::events::log_event(
            st.inner(),
            core_library::Event {
                event_type: "develop.export".into(),
                image_id: Some(image_id),
                process_version: Some(PROCESS_VERSION),
                params_after: serde_json::to_string(&params).ok(),
                context: Some(serde_json::json!({ "format": format }).to_string()),
                ..Default::default()
            },
        );
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Culling ----------

async fn db_write<F>(app: AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(&core_db::rusqlite::Connection) -> Result<(), core_library::LibError>
        + Send
        + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        f(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Best-effort: (re)write the per-image sidecar after a catalog mutation so disk stays the durable
/// source of edit intent. Never fails the command — a sidecar error is logged and swallowed.
fn sync_sidecar(conn: &core_db::rusqlite::Connection, image_id: i64) {
    if let Err(e) = core_library::write_sidecar(conn, image_id) {
        eprintln!("sidecar write failed for image {image_id}: {e}");
    }
}

fn sync_sidecars(conn: &core_db::rusqlite::Connection, image_ids: &[i64]) {
    for &id in image_ids {
        sync_sidecar(conn, id);
    }
}

/// `flag` value → event-type label (for the behavioral log).
fn flag_event_type(flag: &str) -> &'static str {
    match flag {
        "pick" => "culling.flag_pick",
        "reject" => "culling.flag_reject",
        _ => "culling.flag_clear",
    }
}

#[tauri::command]
pub async fn cull_set_rating(
    app: AppHandle,
    image_id: i64,
    stars: i64,
    latency_ms: Option<i64>,
    group_id: Option<String>,
    candidate_ids: Option<Vec<i64>>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_rating(&db.conn, image_id, stars).map_err(|e| e.to_string())?;
        sync_sidecar(&db.conn, image_id);
        let _ = core_library::append_event(
            &db.conn,
            &crate::events::stamp(
                st.inner(),
                core_library::Event {
                    event_type: "culling.rate".into(),
                    image_id: Some(image_id),
                    stars: Some(stars),
                    group_id,
                    candidate_ids: candidate_ids.as_deref().map(core_library::ids_json),
                    latency_ms,
                    ..Default::default()
                },
            ),
        );
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_flag(
    app: AppHandle,
    image_id: i64,
    flag: String,
    latency_ms: Option<i64>,
    group_id: Option<String>,
    candidate_ids: Option<Vec<i64>>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_flag(&db.conn, image_id, &flag).map_err(|e| e.to_string())?;
        sync_sidecar(&db.conn, image_id);
        let _ = core_library::append_event(
            &db.conn,
            &crate::events::stamp(
                st.inner(),
                core_library::Event {
                    event_type: flag_event_type(&flag).into(),
                    image_id: Some(image_id),
                    chosen_id: (flag == "pick").then_some(image_id),
                    flag: Some(flag),
                    group_id,
                    candidate_ids: candidate_ids.as_deref().map(core_library::ids_json),
                    latency_ms,
                    ..Default::default()
                },
            ),
        );
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_label(
    app: AppHandle,
    image_id: i64,
    label: Option<String>,
    latency_ms: Option<i64>,
    group_id: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_label(&db.conn, image_id, label.as_deref()).map_err(|e| e.to_string())?;
        sync_sidecar(&db.conn, image_id);
        let _ = core_library::append_event(
            &db.conn,
            &crate::events::stamp(
                st.inner(),
                core_library::Event {
                    event_type: "culling.label".into(),
                    image_id: Some(image_id),
                    color_label: label,
                    group_id,
                    latency_ms,
                    ..Default::default()
                },
            ),
        );
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// Batch culling — applies one value to a whole selection in a single transaction. The selection IS
// the candidate group, so one event per image carries the shared `group_id` + candidate set.

/// Append one `culling.*` event per image in a batch (same tx). `build` produces the per-image event.
fn log_batch(
    st: &AppState,
    conn: &core_db::rusqlite::Connection,
    image_ids: &[i64],
    group_id: &Option<String>,
    build: impl Fn(i64) -> core_library::Event,
) {
    let cands = core_library::ids_json(image_ids);
    for &id in image_ids {
        let mut e = build(id);
        e.image_id = Some(id);
        e.group_id = group_id.clone();
        e.candidate_ids = Some(cands.clone());
        let _ = core_library::append_event(conn, &crate::events::stamp(st, e));
    }
}

#[tauri::command]
pub async fn cull_set_rating_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    stars: i64,
    group_id: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_rating_many(&mut db.conn, &image_ids, stars).map_err(|e| e.to_string())?;
        sync_sidecars(&db.conn, &image_ids);
        log_batch(st.inner(), &db.conn, &image_ids, &group_id, |_| {
            core_library::Event {
                event_type: "culling.rate".into(),
                stars: Some(stars),
                ..Default::default()
            }
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_flag_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    flag: String,
    group_id: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_flag_many(&mut db.conn, &image_ids, &flag).map_err(|e| e.to_string())?;
        sync_sidecars(&db.conn, &image_ids);
        let et = flag_event_type(&flag);
        log_batch(st.inner(), &db.conn, &image_ids, &group_id, |id| {
            core_library::Event {
                event_type: et.into(),
                chosen_id: (flag == "pick").then_some(id),
                flag: Some(flag.clone()),
                ..Default::default()
            }
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_label_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    label: Option<String>,
    group_id: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_label_many(&mut db.conn, &image_ids, label.as_deref())
            .map_err(|e| e.to_string())?;
        sync_sidecars(&db.conn, &image_ids);
        log_batch(st.inner(), &db.conn, &image_ids, &group_id, |_| {
            core_library::Event {
                event_type: "culling.label".into(),
                color_label: label.clone(),
                ..Default::default()
            }
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Keywords / tags ----------

#[tauri::command]
pub async fn keywords_list(app: AppHandle) -> Result<Vec<KeywordRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_keywords(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keywords_for_image(app: AppHandle, image_id: i64) -> Result<Vec<KeywordRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::keywords_for_image(&db.conn, image_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_add_to_image(
    app: AppHandle,
    image_id: i64,
    name: String,
) -> Result<KeywordRow, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let row =
            core_library::add_keyword_to_image(&db.conn, image_id, &name).map_err(|e| e.to_string())?;
        sync_sidecar(&db.conn, image_id);
        Ok(row)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_add_to_images(
    app: AppHandle,
    image_ids: Vec<i64>,
    name: String,
) -> Result<KeywordRow, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let row = core_library::add_keyword_to_images(&db.conn, &image_ids, &name)
            .map_err(|e| e.to_string())?;
        sync_sidecars(&db.conn, &image_ids);
        Ok(row)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_remove_from_image(
    app: AppHandle,
    image_id: i64,
    keyword_id: i64,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::remove_keyword_from_image(&db.conn, image_id, keyword_id)
            .map_err(|e| e.to_string())?;
        sync_sidecar(&db.conn, image_id);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_delete(app: AppHandle, keyword_id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        // Capture images that carry this keyword BEFORE the cascade delete, to rewrite their sidecars.
        let affected: Vec<i64> = {
            let mut stmt = db
                .conn
                .prepare("SELECT image_id FROM image_keywords WHERE keyword_id = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([keyword_id], |r| r.get::<_, i64>(0))
                .map_err(|e| e.to_string())?;
            rows.filter_map(Result::ok).collect()
        };
        core_library::delete_keyword(&db.conn, keyword_id).map_err(|e| e.to_string())?;
        sync_sidecars(&db.conn, &affected);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Collections ----------

#[tauri::command]
pub async fn collections_list(app: AppHandle) -> Result<Vec<CollectionRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_collections(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collections_for_image(
    app: AppHandle,
    image_id: i64,
) -> Result<Vec<CollectionRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::collections_for_image(&db.conn, image_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_create(
    app: AppHandle,
    name: String,
    is_smart: bool,
    query: Option<String>,
) -> Result<i64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::create_collection(&db.conn, &name, is_smart, query.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_rename(app: AppHandle, id: i64, name: String) -> Result<(), String> {
    db_write(app, move |c| core_library::rename_collection(c, id, &name)).await
}

#[tauri::command]
pub async fn collection_delete(app: AppHandle, id: i64) -> Result<(), String> {
    db_write(app, move |c| core_library::delete_collection(c, id)).await
}

#[tauri::command]
pub async fn collection_add_images(
    app: AppHandle,
    collection_id: i64,
    image_ids: Vec<i64>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::add_images_to_collection(&db.conn, collection_id, &image_ids)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_remove_images(
    app: AppHandle,
    collection_id: i64,
    image_ids: Vec<i64>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::remove_images_from_collection(&db.conn, collection_id, &image_ids)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A sensible default import destination: the parent of the first watched folder (the library root
/// under which dated `YYYY/YYYY-MM-DD` folders live), if any.
#[tauri::command]
pub async fn app_library_root(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let first: Option<String> = db
            .conn
            .query_row("SELECT path FROM folders ORDER BY id LIMIT 1", [], |r| {
                r.get(0)
            })
            .ok();
        Ok::<_, String>(first.and_then(|p| {
            Path::new(&p)
                .parent()
                .map(|parent| parent.display().to_string())
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Dedup ----------

#[tauri::command]
pub async fn dedup_scan(
    app: AppHandle,
    category: String,
) -> Result<Vec<core_dedup::DupGroup>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        match category.as_str() {
            "capture" => core_dedup::find_same_capture(&db.conn),
            _ => core_dedup::find_byte_identical(&db.conn),
        }
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn dedup_resolve(
    app: AppHandle,
    keep_id: i64,
    trash_ids: Vec<i64>,
    // Optional decision context (frontend passes from the DupGroup): the full set shown, the rule's
    // suggested keeper, and the group key. Backward-compatible — absent → derived/None.
    candidate_ids: Option<Vec<i64>>,
    auto_keeper_id: Option<i64>,
    group_id: Option<String>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let res = core_dedup::resolve(&db.conn, keep_id, &trash_ids).map_err(|e| e.to_string())?;
        gc_orphan_thumbs(&db.conn, &st.thumbs, &res.trashed_hashes);

        // Behavioral log: the keeper choice + full candidate set + (if known) the auto-suggested
        // keeper, so we can later learn keeper ranking and detect user overrides.
        let candidates = candidate_ids.unwrap_or_else(|| {
            let mut v = trash_ids.clone();
            v.push(keep_id);
            v.sort_unstable();
            v
        });
        let gid = group_id.or_else(|| candidates.iter().min().map(|m| format!("dedup-{m}")));
        let cands_json = core_library::ids_json(&candidates);
        let ev = |event_type: &str| {
            crate::events::stamp(
                st.inner(),
                core_library::Event {
                    event_type: event_type.into(),
                    group_id: gid.clone(),
                    candidate_ids: Some(cands_json.clone()),
                    suggestion_id: auto_keeper_id,
                    ..Default::default()
                },
            )
        };
        let _ = core_library::append_event(&db.conn, &ev("dedup.group_shown"));
        let _ = core_library::append_event(
            &db.conn,
            &core_library::Event {
                chosen_id: Some(keep_id),
                rejected_ids: Some(core_library::ids_json(&trash_ids)),
                ..ev("dedup.keeper_chosen")
            },
        );
        if auto_keeper_id.is_some_and(|ak| ak != keep_id) {
            let _ = core_library::append_event(
                &db.conn,
                &core_library::Event {
                    chosen_id: Some(keep_id),
                    ..ev("dedup.override")
                },
            );
        }
        Ok::<_, String>(res.trashed)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Auto-resolve all byte-identical duplicate groups (keep one copy each, trash the rest). Returns
/// the number of files trashed. Same-capture/perceptual matches are never auto-resolved.
#[tauri::command]
pub async fn dedup_resolve_bulk(app: AppHandle) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let res = core_dedup::auto_resolve_byte_identical(&db.conn).map_err(|e| e.to_string())?;
        gc_orphan_thumbs(&db.conn, &st.thumbs, &res.trashed_hashes);
        Ok::<_, String>(res.trashed)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Perceptual near-duplicate scan. Lazily computes & persists a dHash for any present image that
/// lacks one (parallel decode of cached thumbnails, emitting `dedup:progress`), then groups images
/// within `threshold` Hamming bits. Never auto-trashes — resolution stays manual per group.
#[tauri::command]
pub async fn dedup_scan_perceptual(
    app: AppHandle,
    threshold: u32,
) -> Result<Vec<core_dedup::DupGroup>, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();

        // Present rows lacking a phash: (id, lowercase-hex content hash for the thumb lookup).
        let todo: Vec<(i64, String)> = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let mut stmt = db
                .conn
                .prepare(
                    "SELECT id, lower(hex(content_hash)) FROM images \
                     WHERE status='present' AND phash IS NULL",
                )
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|e| e.to_string())?
                .collect::<core_db::rusqlite::Result<Vec<(i64, String)>>>()
                .map_err(|e| e.to_string())?;
            rows
        };

        // Compute dHashes in parallel from the cached thumbnails, then persist in one transaction.
        if !todo.is_empty() {
            let total = todo.len();
            let done = AtomicUsize::new(0);
            let computed: Vec<(i64, i64)> = todo
                .par_iter()
                .filter_map(|(id, hex)| {
                    let phash = st
                        .thumbs
                        .read(hex, core_library::THUMB_SIZE)
                        .ok()
                        .and_then(|bytes| core_dedup::dhash_from_jpeg(&bytes));
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == total || n.is_multiple_of(16) {
                        let _ = app2.emit(
                            "dedup:progress",
                            serde_json::json!({"done": n, "total": total}),
                        );
                    }
                    phash.map(|p| (*id, p as i64))
                })
                .collect();

            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            {
                let mut s = tx
                    .prepare("UPDATE images SET phash=?1 WHERE id=?2")
                    .map_err(|e| e.to_string())?;
                for (id, p) in &computed {
                    s.execute(core_db::rusqlite::params![p, id])
                        .map_err(|e| e.to_string())?;
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
        }

        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_dedup::find_perceptual(&db.conn, threshold).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Import ----------

#[tauri::command]
pub async fn import_start(
    app: AppHandle,
    source: String,
    mode: String,
    dest: String,
    recursive: Option<bool>,
) -> Result<core_import::ImportStats, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();
        let import_mode = match mode.as_str() {
            "move" => core_import::ImportMode::Move,
            "reference" => core_import::ImportMode::Reference,
            _ => core_import::ImportMode::Copy,
        };
        // Gate the FS watcher for the duration of the import so it can't race the now-unlocked
        // per-file copy/process phase (double-decode / duplicate insert). Dropped when this closure
        // returns (success or error), which runs one deferred watcher sync if one was suppressed.
        let _import_guard = crate::watch::ImportGuard::new(app2.clone());
        let progress_app = app2.clone();
        // Buffer freshly-imported rows and flush them with the progress event so the frontend can
        // append photos to the grid live (single-threaded blocking import → RefCell is sufficient).
        let pending = std::cell::RefCell::new(Vec::<core_library::ImageRow>::new());
        let stats = core_import::import(
            &st.db,
            &st.thumbs,
            Path::new(&source),
            import_mode,
            Path::new(&dest),
            recursive.unwrap_or(true),
            |done, total, added| {
                if let Some(row) = added {
                    pending.borrow_mut().push(row.clone());
                }
                // Flush on completion, a full batch, or every 4th file (keeps the counter live even
                // through runs of skipped files that add no rows).
                if done == total || pending.borrow().len() >= 8 || done.is_multiple_of(4) {
                    let images: Vec<core_library::ImageRow> =
                        pending.borrow_mut().drain(..).collect();
                    let _ = progress_app.emit(
                        "import:progress",
                        serde_json::json!({"done": done, "total": total, "images": images}),
                    );
                }
            },
        )
        .map_err(|e| e.to_string())?;
        enforce_thumb_cap(&st);
        let _ = app2.emit("import:done", &stats);
        Ok::<_, String>(stats)
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Settings ----------

/// Configured thumbnail-cache cap in bytes (default when unset).
#[tauri::command]
pub async fn thumb_cache_cap(app: AppHandle) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::thumb_cache_cap(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Current on-disk size of the thumbnail cache in bytes.
#[tauri::command]
pub async fn thumb_cache_size(app: AppHandle) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        st.thumbs.total_size().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Persist a new thumbnail-cache cap and immediately evict down to it. Returns bytes freed.
#[tauri::command]
pub async fn set_thumb_cache_cap(app: AppHandle, bytes: u64) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_thumb_cache_cap(&db.conn, bytes).map_err(|e| e.to_string())?;
        }
        st.thumbs.evict_to(bytes).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- AI analysis ----------

/// Total / analyzed / pending image counts + models-ready + running flags (for the Detected panel).
#[tauri::command]
pub async fn analysis_status(app: AppHandle) -> Result<crate::analysis::AnalysisStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        crate::analysis::status(&st)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Download any missing model files (first run only). Emits `analysis:models` progress.
#[tauri::command]
pub async fn analysis_models_ensure(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || crate::analysis::ensure_models(&app))
        .await
        .map_err(|e| e.to_string())?
}

/// Run the background analysis pass. `force` re-analyzes everything. Emits `analysis:progress`/`:done`.
#[tauri::command]
pub async fn analysis_run(
    app: AppHandle,
    force: bool,
) -> Result<crate::analysis::RunStats, String> {
    tauri::async_runtime::spawn_blocking(move || crate::analysis::run_pass(&app, force))
        .await
        .map_err(|e| e.to_string())?
}

/// Request the running analysis pass to stop after the current batch commits. Results already
/// persisted are kept; the pass then emits `analysis:done` with its partial stats. No-op if idle.
#[tauri::command]
pub fn analysis_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.analysis_running.load(Ordering::SeqCst) {
        st.analysis_cancel.store(true, Ordering::SeqCst);
    }
}

/// Detected-object category counts (distinct images) for the LeftNav facet.
#[tauri::command]
pub async fn analysis_facets(app: AppHandle) -> Result<Vec<FacetRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::analysis_facets(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Detected objects for one image (metadata panel).
#[tauri::command]
pub async fn image_detections(app: AppHandle, id: i64) -> Result<Vec<DetectionRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::detections_for_image(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Generated caption + keywords for one image (metadata panel).
#[tauri::command]
pub async fn image_caption(app: AppHandle, id: i64) -> Result<Option<CaptionRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::caption_for_image(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// MobileCLIP presence-probe scores for one image (advisory AI readout; `None` until the probe ran).
#[tauri::command]
pub async fn image_presence(app: AppHandle, id: i64) -> Result<Option<PresenceRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::presence_for_image(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---- Faces / People ----

/// People status: present-image total, how many are face-processed, total faces/people, model state.
#[tauri::command]
pub async fn faces_status(app: AppHandle) -> Result<crate::faces::FacesStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        crate::faces::status(&st)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Download any missing face models (~190 MB, first run only). Emits `faces:models` progress.
#[tauri::command]
pub async fn faces_models_ensure(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || crate::faces::ensure_face_models(&app))
        .await
        .map_err(|e| e.to_string())?
}

/// Run the "Find People" pass (detect → align → embed → cluster). `force` re-processes everything.
/// Emits `faces:progress`/`faces:done`.
#[tauri::command]
pub async fn faces_run(app: AppHandle, force: bool) -> Result<crate::faces::FacesRunStats, String> {
    tauri::async_runtime::spawn_blocking(move || crate::faces::run_pass(&app, force))
        .await
        .map_err(|e| e.to_string())?
}

/// Request the running face pass to stop after the current batch commits. No-op if idle.
#[tauri::command]
pub fn faces_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.faces_running.load(Ordering::SeqCst) {
        st.faces_cancel.store(true, Ordering::SeqCst);
    }
}

/// People for the sidebar (named first, then unnamed "Suggested" clusters); each with a cover crop.
#[tauri::command]
pub async fn people_list(app: AppHandle, include_hidden: bool) -> Result<Vec<PersonRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_people(&db.conn, include_hidden).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Faces of one person, optionally restricted to a status (e.g. `"unconfirmed"` for the Review flow).
#[tauri::command]
pub async fn person_faces(
    app: AppHandle,
    person_id: i64,
    status: Option<String>,
) -> Result<Vec<PersonFaceRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::person_faces(&db.conn, person_id, status.as_deref()).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Faces detected in one image (+ their person names) — the RightInfo "People" chips.
#[tauri::command]
pub async fn image_faces(app: AppHandle, id: i64) -> Result<Vec<ImageFaceRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_faces(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set (or clear with `null`) a person's name.
#[tauri::command]
pub async fn person_set_name(
    app: AppHandle,
    person_id: i64,
    name: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_person_name(
            &db.conn,
            person_id,
            name.as_deref(),
            core_library::now_epoch(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Hide/unhide a person (excluded from the sidebar + library filter when hidden).
#[tauri::command]
pub async fn person_set_hidden(
    app: AppHandle,
    person_id: i64,
    hidden: bool,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_person_hidden(&db.conn, person_id, hidden, core_library::now_epoch())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set a person's cover (key) face. The face must belong to the person.
#[tauri::command]
pub async fn person_set_cover(
    app: AppHandle,
    person_id: i64,
    face_id: i64,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_person_cover(&db.conn, person_id, face_id, core_library::now_epoch())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Merge person `src` into `dst` (move all faces + rejections, delete `src`). Atomic; not reversible.
#[tauri::command]
pub async fn person_merge(app: AppHandle, dst: i64, src: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.transaction().map_err(|e| e.to_string())?;
        core_library::merge_people(&tx, dst, src).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Confirm a face belongs to its person ("yes" in the Review flow).
#[tauri::command]
pub async fn face_confirm(app: AppHandle, face_id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::confirm_face(&db.conn, face_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reject a face from its person ("not this person") — unlink + remember the rejection.
#[tauri::command]
pub async fn face_reject(app: AppHandle, face_id: i64) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::reject_face(&db.conn, face_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Reassign a face to a person (confirmed), or `null` to send it back to the suggestion pool.
#[tauri::command]
pub async fn face_assign(
    app: AppHandle,
    face_id: i64,
    person_id: Option<i64>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::assign_face_person(&db.conn, face_id, person_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Delete ALL face + person data (privacy "Delete all face data"). Atomic; not reversible.
#[tauri::command]
pub async fn faces_delete_all(app: AppHandle) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.transaction().map_err(|e| e.to_string())?;
        core_library::delete_all_face_data(&tx).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Backfill per-image feature vectors (lighting/best-shot/dedup model inputs) for images missing
/// them. Explicit action; emits `features:progress`/`features:done`. Returns the count computed.
#[tauri::command]
pub async fn features_backfill(app: AppHandle) -> Result<usize, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || crate::features::run_backfill(&app2))
        .await
        .map_err(|e| e.to_string())?
}

/// Real per-image histogram for the Library metadata panel, computed from the cached thumbnail (no
/// GPU render needed). `None` if the image/thumb is unavailable.
#[tauri::command]
pub async fn image_histogram(app: AppHandle, image_id: i64) -> Result<Option<Histogram>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let hash = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            match core_library::image_by_id(&db.conn, image_id).map_err(|e| e.to_string())? {
                Some(r) => r.content_hash,
                None => return Ok(None),
            }
        };
        let Ok(jpeg) = st.thumbs.read(&hash, core_library::THUMB_SIZE) else {
            return Ok(None);
        };
        Ok(core_pipeline::histogram_from_jpeg(&jpeg))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Write a sidecar (`<raw>.json`: edits + rating + keywords) next to every present RAW — migrates an
/// existing catalog onto the durable on-disk format. Returns the count written.
#[tauri::command]
pub async fn sidecars_write_all(app: AppHandle) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::write_all_sidecars(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Force-apply every present image's sidecar back into the catalog (recover edits/ratings/keywords
/// after a catalog loss or when moving between machines). Returns the count hydrated.
#[tauri::command]
pub async fn sidecars_rebuild(app: AppHandle) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::rebuild_from_sidecars(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Manual ground-truth labels for one image (the "Contains person/animal" checkboxes).
#[tauri::command]
pub async fn image_user_labels(app: AppHandle, id: i64) -> Result<UserLabels, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::user_labels(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set one manual label field (`field` = "person" | "animal"; `value` = Some(bool) or None to clear).
#[tauri::command]
pub async fn set_image_user_label(
    app: AppHandle,
    id: i64,
    field: String,
    value: Option<bool>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_user_label(&db.conn, id, &field, value, core_library::now_epoch())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set one manual label field on many images at once (multi-select labeling). Logs one behavioral
/// event per image so the labels feed the AI training layer with their full candidate context.
#[tauri::command]
pub async fn set_image_user_label_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    field: String,
    value: Option<bool>,
    group_id: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_user_label_many(
            &mut db.conn,
            &image_ids,
            &field,
            value,
            core_library::now_epoch(),
        )
        .map_err(|e| e.to_string())?;
        let et = format!("label.{field}_set");
        let ctx = format!("{{\"field\":\"{field}\",\"value\":{}}}", match value {
            Some(true) => "true",
            Some(false) => "false",
            None => "null",
        });
        log_batch(st.inner(), &db.conn, &image_ids, &group_id, |_| {
            core_library::Event {
                event_type: et.clone(),
                context: Some(ctx.clone()),
                ..Default::default()
            }
        });
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Configured MegaDetector input size (640 or 1280).
#[tauri::command]
pub async fn analysis_detector_size(app: AppHandle) -> Result<u32, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::animal_detector_size(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Set the MegaDetector input size; invalidates the cached analyzer registry so the next pass
/// rebuilds at the new resolution.
#[tauri::command]
pub async fn set_analysis_detector_size(app: AppHandle, size: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_animal_detector_size(&db.conn, size).map_err(|e| e.to_string())?;
        }
        *st.analyzers.lock().map_err(|e| e.to_string())? = None;
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}
