//! Panorama merge orchestration (Lightroom-style Photo Merge → Panorama).
//!
//! Bridges the pure stitcher (`core-pano`, plain f32 buffers) to the catalog world: decodes the
//! selected sources to camera-native linear RGB (`core_raw::develop_camera_native`), stitches, and
//! writes a 16-bit LinearRaw DNG next to the first source (`core_raw::write_pano_dng`), which then
//! re-enters the catalog exactly like an imported raw (thumbnail from the embedded preview, full
//! WB latitude in Develop).
//!
//! Two entry points mirror the merge dialog: [`preview`] (fast low-res stitch of cached downscaled
//! frames → JPEG bytes for the dialog) and [`merge`] (full-res background job). The job follows the
//! `denoise.rs` shape: `AtomicBool` running/cancel pair + Drop guard, phase events
//! (`panorama:progress {phase}` / `panorama:done {imageId}`), and — the ea0d66a import-freeze
//! lesson — ALL decode/stitch/encode work runs without the DB lock; the mutex is taken only for the
//! brief source-path lookup and the final insert+link transaction.
//!
//! v1 memory note: `merge` holds every source's full-res camera-native buffer while stitching
//! (~0.4 GB per 32 MP frame) plus the blender accumulators. Fine for the 2–10 frame target; a
//! decode-on-demand streaming `Frame` source is the documented follow-up if larger merges matter.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use core_pano::{Frame, Phase, Projection, StitchOptions};
use core_raw::{CameraNativeImage, PanoColorMeta};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Long side of the per-frame decode cache backing the dialog preview. Small enough that ten
/// frames cost ~60 MB total, large enough that registration still has ~1 MP to work with.
const PREVIEW_FRAME_EDGE: u32 = 1400;
/// Canvas cap for the dialog preview stitch (the JPEG sent to the UI is smaller still).
const PREVIEW_CANVAS_EDGE: u32 = 3000;
/// Editable cap for the final composite: the develop pipeline has no tiling and rejects textures
/// above the device limit (16384 on Apple Silicon), and interactive develop downsizes to 8192
/// anyway. 12000 keeps a merged pano openable everywhere with headroom to spare.
const PANO_EDITABLE_CAP: u32 = 12_000;

/// Cached downscaled frames for the merge dialog: one preview decode pass serves every projection /
/// crop toggle without re-reading the raws. Keyed by the exact id list (selection order matters —
/// it becomes `panorama_sources.position`).
pub struct PreviewCache {
    pub ids: Vec<i64>,
    frames: Vec<Frame>,
    metas: Vec<PanoColorMeta>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanoStatus {
    pub running: bool,
}

pub fn status(st: &AppState) -> PanoStatus {
    PanoStatus {
        running: st.panorama_running.load(Ordering::SeqCst),
    }
}

/// Resets running + cancel on drop so an early return / error can never wedge the job gate.
struct RunGuard<'a> {
    running: &'a AtomicBool,
    cancel: &'a AtomicBool,
}
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.cancel.store(false, Ordering::SeqCst);
    }
}

fn phase_name(p: Phase) -> &'static str {
    match p {
        Phase::Register => "register",
        Phase::BundleAdjust => "bundle_adjust",
        Phase::Warp => "warp",
        Phase::Blend => "blend",
        Phase::Crop => "crop",
        Phase::Rectangle => "rectangle",
        Phase::Encode => "encode",
    }
}

fn emit_phase<R: Runtime>(app: &AppHandle<R>, phase: &str) {
    let _ = app.emit("panorama:progress", serde_json::json!({ "phase": phase }));
}

fn parse_projection(s: &str) -> Result<Projection, String> {
    match s.to_ascii_lowercase().as_str() {
        "auto" => Ok(Projection::Auto),
        "spherical" => Ok(Projection::Spherical),
        "cylindrical" => Ok(Projection::Cylindrical),
        "perspective" => Ok(Projection::Perspective),
        other => Err(format!("unknown projection '{other}'")),
    }
}

/// Source paths in selection order, under one brief DB lock.
fn resolve_paths(st: &AppState, ids: &[i64]) -> Result<Vec<String>, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    ids.iter()
        .map(|id| {
            core_library::image_by_id(&db.conn, *id)
                .map_err(|e| e.to_string())?
                .map(|row| row.path)
                .ok_or_else(|| format!("image {id} not found in catalog"))
        })
        .collect()
}

fn validate_ids(ids: &[i64]) -> Result<(), String> {
    if !(2..=10).contains(&ids.len()) {
        return Err(format!(
            "panorama merge needs 2–10 images ({} selected)",
            ids.len()
        ));
    }
    Ok(())
}

/// Refuse mixed-camera merges up front: the output DNG carries ONE camera's color matrix, so
/// frames from different bodies would silently develop with the wrong color.
fn check_same_camera(metas: &[PanoColorMeta]) -> Result<(), String> {
    let first = metas[0].camera_id();
    for m in &metas[1..] {
        if m.camera_id() != first {
            return Err(format!(
                "all frames must come from the same camera (found {} {} and {} {})",
                first.0,
                first.1,
                m.camera_id().0,
                m.camera_id().1
            ));
        }
    }
    Ok(())
}

fn to_frame(native: LinearImage3) -> Frame {
    Frame {
        width: native.width as usize,
        height: native.height as usize,
        rgb: native.data,
        focal_seed_px: None,
    }
}

/// Local alias to keep `to_frame` readable — `CameraNativeImage` minus the meta.
struct LinearImage3 {
    width: u32,
    height: u32,
    data: Vec<f32>,
}

fn split_native(native: CameraNativeImage) -> (LinearImage3, PanoColorMeta) {
    (
        LinearImage3 {
            width: native.width,
            height: native.height,
            data: native.data,
        },
        native.meta,
    )
}

/// Downscale a camera-native buffer (reuses the colorspace-agnostic `LinearImage` resize).
fn downscale_native(img: LinearImage3, max_edge: u32) -> LinearImage3 {
    let li = core_raw::LinearImage {
        width: img.width,
        height: img.height,
        data: img.data,
    }
    .downscale_into(max_edge);
    LinearImage3 {
        width: li.width,
        height: li.height,
        data: li.data,
    }
}

/// Fast low-res stitch for the merge dialog → sRGB JPEG bytes. First call for an id list decodes
/// each source once and caches downscaled frames; subsequent calls (projection/crop changes) reuse
/// them. Runs on the caller's blocking thread; refused while a full merge is running.
pub fn preview<R: Runtime>(
    app: &AppHandle<R>,
    ids: Vec<i64>,
    projection: String,
    boundary_warp: f32,
    auto_crop: bool,
) -> Result<Vec<u8>, String> {
    let st = app.state::<AppState>();
    validate_ids(&ids)?;
    if st.panorama_running.load(Ordering::SeqCst) {
        return Err("a panorama merge is already running".into());
    }
    let projection = parse_projection(&projection)?;

    // Serialize preview work (and cache access) on one mutex — a second preview waits rather than
    // duplicating a multi-second decode burst.
    let mut cache = st.panorama_preview_cache.lock().map_err(|e| e.to_string())?;
    if cache.as_ref().map(|c| &c.ids) != Some(&ids) {
        let paths = resolve_paths(&st, &ids)?;
        let mut frames = Vec::with_capacity(paths.len());
        let mut metas = Vec::with_capacity(paths.len());
        for path in &paths {
            let src = core_raw::source_from_path(Path::new(path)).map_err(|e| e.to_string())?;
            let native = core_raw::develop_camera_native(&src).map_err(|e| e.to_string())?;
            let (img, meta) = split_native(native);
            frames.push(to_frame(downscale_native(img, PREVIEW_FRAME_EDGE)));
            metas.push(meta);
        }
        check_same_camera(&metas)?;
        *cache = Some(PreviewCache { ids: ids.clone(), frames, metas });
    }
    let c = cache.as_ref().expect("cache filled above");

    let opt = StitchOptions {
        projection,
        boundary_warp,
        auto_crop,
        max_long_side: PREVIEW_CANVAS_EDGE,
        preview: true,
    };
    let result = core_pano::stitch(&c.frames, &opt, &|_| {}).map_err(|e| e.to_string())?;
    let meta = &c.metas[result.reference_index];
    core_raw::native_to_srgb_jpeg(
        result.width as u32,
        result.height as u32,
        &result.rgb,
        meta,
        2048,
        82,
    )
    .map_err(|e| e.to_string())
}

/// Non-colliding `<first-source-stem>-Pano[-N].dng` next to the first source.
fn pano_dest_path(first_source: &Path) -> Result<PathBuf, String> {
    let dir = first_source
        .parent()
        .ok_or_else(|| "source has no parent directory".to_string())?;
    let stem = first_source
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("merge");
    let first = dir.join(format!("{stem}-Pano.dng"));
    if !first.exists() {
        return Ok(first);
    }
    for n in 2..1000 {
        let p = dir.join(format!("{stem}-Pano-{n}.dng"));
        if !p.exists() {
            return Ok(p);
        }
    }
    Err("could not find a free panorama filename".into())
}

/// Full-resolution merge: decode → stitch → LinearRaw DNG → catalog insert + source links.
/// Returns the new catalog image id. Emits `panorama:progress`/`panorama:done`; cancellation is
/// honored between sources/phases (a mid-phase stitch completes its phase first).
pub fn merge<R: Runtime>(
    app: &AppHandle<R>,
    ids: Vec<i64>,
    projection: String,
    boundary_warp: f32,
    auto_crop: bool,
) -> Result<i64, String> {
    let st = app.state::<AppState>();
    validate_ids(&ids)?;
    let projection = parse_projection(&projection)?;
    if st.panorama_running.swap(true, Ordering::SeqCst) {
        return Err("a panorama merge is already running".into());
    }
    let _guard = RunGuard {
        running: &st.panorama_running,
        cancel: &st.panorama_cancel,
    };
    st.panorama_cancel.store(false, Ordering::SeqCst);

    let cancelled = || st.panorama_cancel.load(Ordering::SeqCst);

    // Brief lock: resolve paths + the first source's folder (the pano lands in the same folder).
    let paths = resolve_paths(&st, &ids)?;
    let folder_id: i64 = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        db.conn
            .query_row(
                "SELECT folder_id FROM images WHERE id = ?1",
                [ids[0]],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?
    };

    // Decode every source full-res (unlocked). Sequential on purpose: each decode is internally
    // parallel (rawler) and the buffers are the dominant memory cost.
    emit_phase(app, "register");
    let mut frames = Vec::with_capacity(paths.len());
    let mut metas = Vec::with_capacity(paths.len());
    for path in &paths {
        if cancelled() {
            return Err("cancelled".into());
        }
        let src = core_raw::source_from_path(Path::new(path)).map_err(|e| e.to_string())?;
        let native = core_raw::develop_camera_native(&src)
            .map_err(|e| format!("{path}: {e}"))?;
        let (img, meta) = split_native(native);
        frames.push(to_frame(img));
        metas.push(meta);
    }
    check_same_camera(&metas)?;

    // The composite must stay openable in the (tiling-free) GPU develop pipeline.
    let device_cap = st
        .gpu
        .as_ref()
        .map(|g| g.ctx.max_texture_dim)
        .unwrap_or(PANO_EDITABLE_CAP);
    let opt = StitchOptions {
        projection,
        boundary_warp,
        auto_crop,
        max_long_side: PANO_EDITABLE_CAP.min(device_cap),
        preview: false,
    };

    let app_for_progress = app.clone();
    let result = core_pano::stitch(&frames, &opt, &move |phase| {
        emit_phase(&app_for_progress, phase_name(phase));
    })
    .map_err(|e| e.to_string())?;
    drop(frames);
    if cancelled() {
        return Err("cancelled".into());
    }

    // Author the DNG next to the first source (unlocked).
    emit_phase(app, "encode");
    let dest = pano_dest_path(Path::new(&paths[0]))?;
    let ref_meta = &metas[result.reference_index];
    core_raw::write_pano_dng(
        &dest,
        result.width as u32,
        result.height as u32,
        &result.rgb,
        ref_meta,
    )
    .map_err(|e| e.to_string())?;
    if cancelled() {
        let _ = std::fs::remove_file(&dest);
        return Err("cancelled".into());
    }

    // Register in the catalog: heavy hash/thumbnail work unlocked, then one short transaction for
    // the image row + source links (the ea0d66a discipline).
    let processed = core_library::process_file(&dest, &st.thumbs, 512).map_err(|e| {
        let _ = std::fs::remove_file(&dest);
        e.to_string()
    })?;
    let new_id: i64 = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let id = core_library::insert_image(&tx, folder_id, core_library::now_epoch(), &processed)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "merged panorama is already in the catalog".to_string())?;
        for (pos, source_id) in ids.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO panorama_sources (pano_image_id, source_image_id, position)
                 VALUES (?1, ?2, ?3)",
                core_db::rusqlite::params![id, source_id, pos as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        id
    };

    // Free the dialog's cached preview frames — the merge is done, the memory matters more.
    if let Ok(mut cache) = st.panorama_preview_cache.lock() {
        *cache = None;
    }

    let _ = app.emit("library:changed", ());
    let _ = app.emit("panorama:done", serde_json::json!({ "imageId": new_id }));
    Ok(new_id)
}

pub fn cancel(st: &AppState) {
    if st.panorama_running.load(Ordering::SeqCst) {
        st.panorama_cancel.store(true, Ordering::SeqCst);
    }
}

/// Drop the dialog's cached frames (modal closed / selection changed away).
pub fn clear_preview_cache(st: &AppState) {
    if let Ok(mut cache) = st.panorama_preview_cache.lock() {
        *cache = None;
    }
}

/// Placeholder so `Mutex<Option<PreviewCache>>` has a home in `AppState` without a pub type leak.
pub type PreviewCacheSlot = Mutex<Option<PreviewCache>>;
