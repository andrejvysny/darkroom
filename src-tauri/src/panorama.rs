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
//! Memory: `merge` never holds more than ONE full-res camera-native buffer. It hands the stitcher a
//! [`CatalogFrameSource`] (`core_pano::FrameSource`), which decodes on demand — every source is
//! materialized once downscaled for registration/exposure/seam work, then once at full resolution
//! during the blend, dropped before the next is read. Peak is therefore ~0.4 GB (one 32 MP decode)
//! plus ~16 MB per source for the low-res pass, instead of ~0.4 GB × N.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use core_pano::{Frame, FrameSource, LoadedFrame, PanoError, Phase, Projection, StitchOptions};
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
    for m in &metas[1..] {
        same_camera(&metas[0], m)?;
    }
    Ok(())
}

/// One-against-the-reference form of [`check_same_camera`], for the streaming source (which sees
/// frames one at a time and compares each against the first it decoded).
fn same_camera(reference: &PanoColorMeta, other: &PanoColorMeta) -> Result<(), String> {
    let first = reference.camera_id();
    if other.camera_id() != first {
        return Err(format!(
            "all frames must come from the same camera (found {} {} and {} {})",
            first.0,
            first.1,
            other.camera_id().0,
            other.camera_id().1
        ));
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
///
/// **Lanczos3, not the cheaper Triangle**: these buffers are what registration extracts features
/// from, and a ~5× Triangle reduction aliases enough to cost real matches. Measured on a 14-frame
/// R7 sweep, Triangle lows put only 8 frames in the largest component where full-res registration
/// found 10 — i.e. two frames silently dropped out of the panorama. The extra filter cost is
/// nothing next to the RAW decode that produced the buffer.
fn downscale_native(img: LinearImage3, max_edge: u32) -> LinearImage3 {
    let li = core_raw::LinearImage {
        width: img.width,
        height: img.height,
        data: img.data,
    }
    .downscale_into_hq(max_edge);
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
    let mut cache = st
        .panorama_preview_cache
        .lock()
        .map_err(|e| e.to_string())?;
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
        *cache = Some(PreviewCache {
            ids: ids.clone(),
            frames,
            metas,
        });
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

/// Decode-on-demand [`FrameSource`] over catalog paths — the reason a merge's peak memory is one
/// full-res frame rather than all of them.
///
/// Side effect by design: every `load` records that frame's [`PanoColorMeta`] (the color matrix /
/// WB / EXIF the output DNG must carry) and, from the second frame on, rejects a body that doesn't
/// match the first one decoded. Because the stitcher's low-resolution pass reads every frame before
/// any compositing starts, a mixed-camera selection still fails fast — the same guarantee the old
/// eager decode loop gave via `check_same_camera`.
struct CatalogFrameSource {
    paths: Vec<String>,
    metas: Mutex<Vec<Option<PanoColorMeta>>>,
}

impl CatalogFrameSource {
    fn new(paths: Vec<String>) -> CatalogFrameSource {
        let metas = (0..paths.len()).map(|_| None).collect();
        CatalogFrameSource {
            paths,
            metas: Mutex::new(metas),
        }
    }

    fn record_meta(&self, i: usize, meta: PanoColorMeta) -> Result<(), PanoError> {
        let mut metas = self
            .metas
            .lock()
            .map_err(|e| PanoError::Load(e.to_string()))?;
        if let Some(first) = metas.iter().flatten().next() {
            same_camera(first, &meta).map_err(PanoError::Load)?;
        }
        metas[i] = Some(meta);
        Ok(())
    }

    /// The metadata captured during the low-resolution pass, consumed after the stitch.
    fn into_metas(self) -> Result<Vec<Option<PanoColorMeta>>, String> {
        self.metas.into_inner().map_err(|e| e.to_string())
    }
}

impl FrameSource for CatalogFrameSource {
    fn len(&self) -> usize {
        self.paths.len()
    }

    fn load(&self, i: usize, max_long_side: Option<u32>) -> Result<LoadedFrame, PanoError> {
        let path = self
            .paths
            .get(i)
            .ok_or_else(|| PanoError::Load(format!("frame index {i} out of range")))?;
        let src = core_raw::source_from_path(Path::new(path))
            .map_err(|e| PanoError::Load(format!("{path}: {e}")))?;
        let native = core_raw::develop_camera_native(&src)
            .map_err(|e| PanoError::Load(format!("{path}: {e}")))?;
        let (img, meta) = split_native(native);
        let (full_width, full_height) = (img.width as usize, img.height as usize);
        self.record_meta(i, meta)?;
        let img = match max_long_side {
            Some(edge) => downscale_native(img, edge),
            None => img,
        };
        Ok(LoadedFrame {
            width: img.width as usize,
            height: img.height as usize,
            rgb: img.data,
            full_width,
            full_height,
            focal_seed_px: None,
        })
    }
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

/// Full-resolution merge: stream-decode → stitch → LinearRaw DNG → catalog insert + source links.
/// Returns the new catalog image id. Emits `panorama:progress`/`panorama:done`.
///
/// The sources are never all resident: a [`CatalogFrameSource`] feeds the stitcher, which decodes
/// each frame small for registration/seams and then one at a time at full resolution for the blend.
/// Cancellation is polled throughout — between source loads, inside registration's parallel sweeps,
/// and between blended frames — so a stop request lands within a frame rather than a whole phase.
pub fn merge<R: Runtime>(
    app: &AppHandle<R>,
    ids: Vec<i64>,
    projection: String,
    boundary_warp: f32,
    auto_crop: bool,
    detect_group_id: Option<i64>,
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

    // Borrow the flag itself (not the `State` wrapper) so the closure is `Sync` — core-pano polls it
    // from inside its rayon-parallel phases.
    let cancel_flag = &st.panorama_cancel;
    let cancelled = move || cancel_flag.load(Ordering::SeqCst);

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

    // Stitch straight off the catalog paths: core-pano pulls each source in (low-res first, then
    // full-res one at a time during the blend) instead of us decoding them all up front.
    let app_for_progress = app.clone();
    let source = CatalogFrameSource::new(paths.clone());
    let result = core_pano::stitch_streaming(
        &source,
        &opt,
        &move |phase| emit_phase(&app_for_progress, phase_name(phase)),
        &cancelled,
    )
    .map_err(|e| e.to_string())?;
    if cancelled() {
        return Err("cancelled".into());
    }

    // Author the DNG next to the first source (unlocked).
    emit_phase(app, "encode");
    let dest = pano_dest_path(Path::new(&paths[0]))?;
    let metas = source.into_metas()?;
    let ref_meta = metas[result.reference_index]
        .as_ref()
        .ok_or_else(|| "reference frame metadata missing".to_string())?;
    core_raw::write_pano_dng(
        &dest,
        result.width as u32,
        result.height as u32,
        &result.rgb,
        ref_meta,
    )
    .map_err(|e| {
        // write_pano_dng creates the file up front, so a failed write can leave a truncated DNG.
        let _ = std::fs::remove_file(&dest);
        e.to_string()
    })?;
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
    let new_id: i64 = (|| -> Result<i64, String> {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let id = core_library::insert_image(&tx, folder_id, core_library::now_epoch(), &processed)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "merged panorama is already in the catalog".to_string())?;
        // Only link frames the stitcher actually used — registration can drop non-overlapping
        // frames from `ids`, and a source row for a frame that isn't in the output would lie about
        // what went into the pano.
        for (pos, &idx) in result.used_indices.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO panorama_sources (pano_image_id, source_image_id, position)
                 VALUES (?1, ?2, ?3)",
                core_db::rusqlite::params![id, ids[idx], pos as i64],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(id)
    })()
    .inspect_err(|_| {
        let _ = std::fs::remove_file(&dest);
    })?;

    // Free the dialog's cached preview frames — the merge is done, the memory matters more.
    if let Ok(mut cache) = st.panorama_preview_cache.lock() {
        *cache = None;
    }

    let _ = app.emit("library:changed", ());
    let _ = app.emit(
        "panorama:done",
        serde_json::json!({
            "imageId": new_id,
            "detectGroupId": detect_group_id,
            "used": result.used_indices.len(),
            "total": ids.len(),
        }),
    );
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
