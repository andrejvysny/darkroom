//! Background AI analysis pass: decode → run analyzers (rayon, no DB lock) → bulk-insert results.
//!
//! Lives here (not in `core-library`) because it bridges the ML crate (`core-analyze`) and the
//! catalog (`core-library`), keeping `core-library` free of any ONNX/ort dependency. Mirrors the
//! indexing pass's discipline: parallel unlocked work, then one brief locked transaction.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use core_analyze::models::{
    ModelStore, ANIMAL_DETECTOR_FILES, CAPTION_FILES, DETECTOR_FILES, VERIFIER_FILES,
};
use core_analyze::{
    AnalysisCtx, Analyzer, AnalyzerRegistry, Captioner, MegaDetector, ObjectDetector,
    PresenceProbe, Verifier, CAPTION_ID,
};
use core_library::{
    apply_cluster_plan, cluster_snapshot, existing_analysis, has_dirty_faces, insert_analysis,
    plan_clusters, present_image_count, present_images, present_targets_after, present_targets_in,
    reconcile_faces, record_attempts, scope_image_ids, stale_count, stale_count_in, stale_targets,
    stale_targets_in, AnalysisInput, ClusterParams, FaceInput, ScanScope, ScopeCounts,
    StageAttempt, StageId, StageSpec, StaleTarget, SCOPE_CHUNK,
};
use image::imageops::FilterType;
use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::faces::{
    analyzer as build_face_analyzer, faces_models_ready, to_input, FACE_ANALYZER_ID,
    FACE_DECODE_EDGE, FACE_MODEL_TAG, FACE_MODEL_VERSION,
};
use crate::state::AppState;

/// Model-version tags stored per result row; bump to force re-analysis of all images.
/// v2: precision-gated decode (per-category thresholds + confidence floor + margin gate + box-sanity),
/// Animals removed from D-FINE (now MegaDetector), MLProgram CoreML format.
/// v3: label-calibrated People recall — floor 0.50→0.40 + People gate 0.55→0.40 with precision moved
/// to a strict per-category person verifier-accept (0.91); measured person F1 0.868→~0.89 on labels.
pub const DETECTOR_VERSION: &str = "dfine-m-coco-v3";
pub const CAPTION_VERSION: &str = "florence2-base-ft-q4f16-v1";
/// MegaDetector version is resolution-specific, so changing the size re-analyzes.
pub const ANIMAL_DETECTOR_VERSION_1280: &str = "mdv5a-1280-v1";
pub const ANIMAL_DETECTOR_VERSION_640: &str = "mdv5a-640-v1";
/// MobileCLIP linear-probe presence classifier (full-image scene scores). Bump when the bundled
/// `presence_probe.json` weights are regenerated.
pub const PRESENCE_VERSION: &str = "mobileclip-s1-probe-v1";

/// Longest-edge the analysis decode is downscaled to (boxes are normalized, so this is loss-only).
const ANALYZE_EDGE: u32 = 1024;

/// Images per commit. Each batch is decoded + inferred in parallel (no DB lock), then written in
/// one short transaction — so results become visible incrementally and an interrupted run keeps
/// everything finished so far. Small enough for prompt partial results, large enough to amortize
/// the lock + transaction overhead.
const ANALYSIS_BATCH: usize = 8;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisStatus {
    pub total: i64,
    pub analyzed: i64,
    pub pending: i64,
    pub models_ready: bool,
    pub running: bool,
    /// Configured AI accelerator (CoreML / DirectML / CPU). A runtime CPU fallback shows as an ort
    /// `warn` in the log rather than changing this value.
    pub accelerator: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStats {
    pub analyzed: usize,
    pub failed: usize,
}

/// True once every detector + animal-detector + caption + verifier model file is present.
pub fn models_ready(st: &AppState) -> bool {
    let store = ModelStore::new(st.models_dir.clone());
    store.has_all(DETECTOR_FILES)
        && store.has_all(ANIMAL_DETECTOR_FILES)
        && store.has_all(CAPTION_FILES)
        && store.has_all(VERIFIER_FILES)
}

/// Whether the models ONE stage needs are present.
///
/// Deliberately per-stage rather than the all-or-nothing [`models_ready`]: with per-stage selection,
/// a missing Florence checkpoint must not make object detection unavailable, and vice versa. Objects
/// and Animals both need the shared CLIP verifier, which is why it appears twice.
pub fn stage_models_ready(st: &AppState, stage: StageId) -> bool {
    let store = ModelStore::new(st.models_dir.clone());
    match stage {
        StageId::Objects => store.has_all(DETECTOR_FILES) && store.has_all(VERIFIER_FILES),
        StageId::Animals => store.has_all(ANIMAL_DETECTOR_FILES) && store.has_all(VERIFIER_FILES),
        StageId::Captions => store.has_all(CAPTION_FILES),
        StageId::Faces => faces_models_ready(st),
        // Panorama detection is pure geometry over cached thumbnails — no model to download.
        StageId::Panoramas => true,
    }
}

/// Every model file across all Detection & Scene analyzers, in download order.
fn analysis_files() -> Vec<core_analyze::models::RemoteFile> {
    DETECTOR_FILES
        .iter()
        .chain(ANIMAL_DETECTOR_FILES)
        .chain(CAPTION_FILES)
        .chain(VERIFIER_FILES)
        .copied()
        .collect()
}

/// Download any missing model files with byte-level `analysis:models` progress + cancellation.
pub fn ensure_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let _guard = st
        .analysis_download_lock
        .lock()
        .map_err(|e| e.to_string())?;
    st.analysis_dl_cancel.store(false, Ordering::SeqCst);
    let store = ModelStore::new(st.models_dir.clone());
    let files = analysis_files();
    let cancel = || st.analysis_dl_cancel.load(Ordering::SeqCst);
    store
        .ensure_with(
            &files,
            |p| crate::model_mgmt::emit_dl(app, "analysis:models", &p),
            &cancel,
        )
        .map_err(|e| e.to_string())
}

/// Manager overview for the Detection & Scene capability.
pub fn overview(st: &AppState) -> crate::model_mgmt::ModelGroup {
    use crate::model_mgmt::{file_info, ModelGroup};
    let store = ModelStore::new(st.models_dir.clone());
    let files = analysis_files();
    let installed = models_ready(st);
    let approx_total: u64 = files.iter().map(|f| f.approx_size).sum();
    let installed_bytes = store.installed_bytes(&files);
    ModelGroup {
        id: "analysis".into(),
        name: "Detection & Scene".into(),
        description: "Detects objects, animals & scenes and writes captions/keywords for search."
            .into(),
        available: true,
        installed,
        size_bytes: if installed {
            installed_bytes
        } else {
            approx_total
        },
        approx_total_bytes: approx_total,
        license: None,
        files: files.iter().map(|f| file_info(&store, f)).collect(),
        tiers: Vec::new(),
        active_tier: None,
        accelerator: core_analyze::accelerator().to_string(),
    }
}

/// Delete the Detection & Scene model files and drop the cached analyzer registry so a re-download
/// rebuilds cleanly.
pub fn remove(st: &AppState) -> Result<(), String> {
    let store = ModelStore::new(st.models_dir.clone());
    store.remove(&analysis_files()).map_err(|e| e.to_string())?;
    *st.analyzers.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}

/// Bitmask identifying which Phase-A analyzers a cached registry was built with, so a run that
/// selects a different set rebuilds instead of silently reusing the wrong one.
fn phase_a_mask(stages: &[StageId]) -> u8 {
    let mut m = 0u8;
    if stages.contains(&StageId::Objects) {
        m |= 1;
    }
    if stages.contains(&StageId::Animals) {
        m |= 2;
    }
    m
}

/// Build (and cache) the **Phase-A** analyzer registry for exactly the selected stages.
///
/// Only the requested detectors are constructed: selecting Captions alone must not load D-FINE and
/// MegaDetector, which is the whole point of per-stage selection. The CLIP verifier is built when
/// either detector is present — both use it, and the presence probe reuses its vision encoder for
/// free, which is why presence has no separate stage.
///
/// The captioner (Florence-2, ~280 MB) is NOT here — it's built lazily in Phase B via
/// [`build_captioner`] so it never sits in memory during the detection+faces phase.
fn registry(st: &AppState, stages: &[StageId]) -> Result<Arc<AnalyzerRegistry>, String> {
    let mask = phase_a_mask(stages);
    if let Some((cached_mask, r)) = st.analyzers.lock().map_err(|e| e.to_string())?.as_ref() {
        if *cached_mask == mask {
            return Ok(r.clone());
        }
    }
    let want_objects = mask & 1 != 0;
    let want_animals = mask & 2 != 0;
    if want_objects && !stage_models_ready(st, StageId::Objects) {
        return Err("object-detection models not downloaded".into());
    }
    if want_animals && !stage_models_ready(st, StageId::Animals) {
        return Err("animal-detection models not downloaded".into());
    }
    let store = ModelStore::new(st.models_dir.clone());
    let mut reg = AnalyzerRegistry::new();
    if !want_objects && !want_animals {
        // No Phase-A work selected (e.g. captions-only): skip the verifier load entirely.
        let arc = Arc::new(reg);
        *st.analyzers.lock().map_err(|e| e.to_string())? = Some((mask, arc.clone()));
        return Ok(arc);
    }
    // Shared CLIP verifier — crop re-check that drops confident-but-wrong detections.
    let (v_vision, v_text, v_tok) = store.verifier_paths();
    let verifier = Arc::new(Verifier::new(&v_vision, &v_text, &v_tok).map_err(|e| e.to_string())?);
    // MegaDetector resolution is a user setting; its version encodes the size so a change re-analyzes.
    let an_size = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::animal_detector_size(&db.conn).map_err(|e| e.to_string())?
    };
    let an_ver = if an_size == 640 {
        ANIMAL_DETECTOR_VERSION_640
    } else {
        ANIMAL_DETECTOR_VERSION_1280
    };
    if want_objects {
        reg.register(Arc::new(
            ObjectDetector::new(&store.detector_path(), DETECTOR_VERSION)
                .map_err(|e| e.to_string())?
                .with_verifier(verifier.clone()),
        ));
    }
    if want_animals {
        reg.register(Arc::new(
            MegaDetector::new(&store.animal_detector_path(), an_ver, an_size)
                .map_err(|e| e.to_string())?
                .with_verifier(verifier.clone()),
        ));
    }
    // Full-image linear-probe presence classifier — reuses the already-built CLIP verifier (vision
    // encoder), so no extra model load. Catches subjects the box detectors miss; fused at query time.
    reg.register(Arc::new(
        PresenceProbe::new(verifier.clone(), PRESENCE_VERSION).map_err(|e| e.to_string())?,
    ));
    let arc = Arc::new(reg);
    *st.analyzers.lock().map_err(|e| e.to_string())? = Some((mask, arc.clone()));
    Ok(arc)
}

/// Build the captioner (Florence-2, ~280 MB / 5 ONNX sessions) on demand for the deferred Phase B.
/// Built fresh per run and dropped when the caller's `Arc` falls out of scope, so Florence is resident
/// ONLY during captioning — never during the detection+faces phase or between scans.
fn build_captioner(st: &AppState) -> Result<Arc<Captioner>, String> {
    if !stage_models_ready(st, StageId::Captions) {
        return Err("caption models not downloaded".into());
    }
    let store = ModelStore::new(st.models_dir.clone());
    let florence = store.florence_dir();
    Ok(Arc::new(
        Captioner::new(&florence, &florence.join("tokenizer.json"), CAPTION_VERSION)
            .map_err(|e| e.to_string())?,
    ))
}

/// Downscale so the longest edge ≤ `edge` (no-op if already within). Boxes are normalized, so this is
/// loss-only.
fn downscale(img: image::RgbImage, edge: u32) -> image::RgbImage {
    let m = img.width().max(img.height());
    if m > edge {
        let s = edge as f32 / m as f32;
        image::imageops::resize(
            &img,
            (img.width() as f32 * s) as u32,
            (img.height() as f32 * s) as u32,
            FilterType::Triangle,
        )
    } else {
        img
    }
}

/// Decode the embedded preview **once** and derive the views the unified pass needs: the sensor-native
/// view (≤ [`ANALYZE_EDGE`]) for the object detectors, and — when `want_oriented` — the EXIF-uprighted
/// view (≤ [`FACE_DECODE_EDGE`]) for faces. Pixel-equivalent to the former separate `preview_image` /
/// `oriented_preview` decoders (guaranteed by core-raw's `decode_once` test), so neither model needs
/// re-validation; we just stop decoding the JPEG twice.
fn decode_shared(
    path: &str,
    want_oriented: bool,
) -> Option<(image::RgbImage, Option<image::RgbImage>)> {
    let src = core_raw::source_from_path(Path::new(path)).ok()?;
    let (mut img, orientation) = core_raw::preview_with_orientation(&src).ok()?;
    let native = downscale(img.to_rgb8(), ANALYZE_EDGE);
    let oriented = if want_oriented {
        if let Some(o) = orientation {
            img.apply_orientation(o);
        }
        Some(downscale(img.to_rgb8(), FACE_DECODE_EDGE))
    } else {
        None
    };
    Some((native, oriented))
}

/// Emit unified scan progress on the single `analysis:progress` stream (`{phase,done,total}`). The
/// People UI listens here too — there is no separate `faces:*` scan event (only `faces:models` for
/// the model download).
fn emit_progress<R: Runtime>(app: &AppHandle<R>, phase: &str, done: usize, total: i64) {
    let _ = app.emit(
        "analysis:progress",
        serde_json::json!({ "phase": phase, "done": done, "total": total }),
    );
}

/// Where a pass pulls its next page of work from.
///
/// `Library` is the original keyset walk over every present image. `Scope` walks a **pre-resolved**
/// id list — the images the grid was showing when the user pressed the button. Resolving that list
/// once at run start (rather than re-querying the filter per page) is what makes a running job
/// immune to the user changing the filter mid-scan.
enum TargetSource<'a> {
    Library {
        cursor: i64,
    },
    Scope {
        ids: &'a [i64],
        /// Index of the next id to pull into `buf`.
        next: usize,
        /// Rows fetched by the last `SCOPE_CHUNK` query, not yet handed to the caller.
        buf: std::collections::VecDeque<StaleTarget>,
    },
}

impl<'a> TargetSource<'a> {
    fn new(scope_ids: Option<&'a [i64]>) -> Self {
        match scope_ids {
            Some(ids) => TargetSource::Scope {
                ids,
                next: 0,
                buf: std::collections::VecDeque::new(),
            },
            None => TargetSource::Library { cursor: 0 },
        }
    }
}

/// One page of work for `specs`. `force` re-scans every image in the source (all stages stale);
/// otherwise only images with ≥1 stale stage. An empty return means the source is exhausted.
///
/// The two sources differ in what "empty" means, and that difference is the whole reason the scoped
/// branch loops: `stale_targets` filters staleness *inside* the keyset window, so an empty keyset
/// page really is the end. `stale_targets_in` is handed a fixed slice of ids, so an all-clean slice
/// yields nothing while later ids may still be dirty — it must keep pulling chunks. Chunks are
/// `SCOPE_CHUNK`-sized (not `limit`-sized) so an already-analyzed scope costs one query per 400
/// images instead of one per batch; the surplus rows wait in `buf`.
fn page_targets(
    st: &AppState,
    specs: &[StageSpec],
    src: &mut TargetSource<'_>,
    limit: usize,
    force: bool,
) -> Result<Vec<StaleTarget>, String> {
    let n = specs.len();
    let all_stale = |t: core_library::AnalyzeTarget| StaleTarget {
        id: t.id,
        path: t.path,
        content_hash_hex: t.content_hash_hex,
        stale: vec![true; n],
    };
    let db = st.db.lock().map_err(|e| e.to_string())?;
    match src {
        TargetSource::Library { cursor } => {
            let page: Vec<StaleTarget> = if force {
                present_targets_after(&db.conn, *cursor, limit as i64)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(all_stale)
                    .collect()
            } else {
                stale_targets(&db.conn, specs, *cursor, limit as i64).map_err(|e| e.to_string())?
            };
            if let Some(last) = page.last() {
                *cursor = last.id;
            }
            Ok(page)
        }
        TargetSource::Scope { ids, next, buf } => {
            while buf.is_empty() && *next < ids.len() {
                let end = (*next + SCOPE_CHUNK).min(ids.len());
                let chunk = &ids[*next..end];
                *next = end;
                let rows: Vec<StaleTarget> = if force {
                    present_targets_in(&db.conn, chunk)
                        .map_err(|e| e.to_string())?
                        .into_iter()
                        .map(all_stale)
                        .collect()
                } else {
                    stale_targets_in(&db.conn, specs, chunk).map_err(|e| e.to_string())?
                };
                buf.extend(rows);
            }
            Ok(buf.drain(..limit.min(buf.len())).collect())
        }
    }
}

/// `Err(msg)` = face inference errored → no marker is written, so the image retries next run (NOT a
/// swallowed zero-face success). `None` = the face stage was not needed/disabled for this image.
type FaceOut = Option<Result<Vec<FaceInput>, String>>;

/// One stage's outcome for one photo, owned so it can cross the rayon boundary.
struct OwnedAttempt {
    stage_id: String,
    model_version: String,
    error: Option<String>,
}

/// What one VISITED photo produced in a batch: results to persist, plus the attempt record for
/// every stage that was actually run (including the ones that failed).
struct PhotoOutcome {
    id: i64,
    inputs: Vec<AnalysisInput>,
    face: FaceOut,
    attempts: Vec<OwnedAttempt>,
}

impl PhotoOutcome {
    fn borrowed_attempts(&self) -> Vec<StageAttempt<'_>> {
        self.attempts
            .iter()
            .map(|a| StageAttempt {
                stage_id: &a.stage_id,
                model_version: &a.model_version,
                error: a.error.as_deref(),
            })
            .collect()
    }

    /// True when at least one stage succeeded — the definition of "analyzed" for run stats.
    fn produced_work(&self) -> bool {
        !self.inputs.is_empty() || matches!(self.face, Some(Ok(_)))
    }
}

/// Progress denominator for one pass: how many images it will visit. `force` counts every image in
/// the source; otherwise only those with ≥1 stale stage.
fn pass_total(
    st: &AppState,
    specs: &[StageSpec],
    scope_ids: Option<&[i64]>,
    force: bool,
) -> Result<i64, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    match (scope_ids, force) {
        (Some(ids), true) => Ok(ids.len() as i64),
        (Some(ids), false) => stale_count_in(&db.conn, specs, ids).map_err(|e| e.to_string()),
        (None, true) => present_image_count(&db.conn).map_err(|e| e.to_string()),
        (None, false) => stale_count(&db.conn, specs).map_err(|e| e.to_string()),
    }
}

/// Resets the `analysis_running` + `analysis_cancel` flags on drop (so an early return / error /
/// cancel can't wedge the guard or leave a stale cancel request for the next run).
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

/// Run the unified AI scan. **Phase A** (detection + faces) runs first for fast feedback, then
/// clustering, then **Phase B** (captions, deferred so Florence stays off the fast path). Per-stage
/// dirty-DAG: each image runs only its stale stages, paged by keyset so the library is never
/// materialized. `force` re-runs every stage on every present image. One run-guard + `analysis_cancel`
/// govern the whole job; progress + completion ride a single `analysis:*` event stream.
///
/// `scope` narrows the pass to the container the user is browsing (folder / capture date / collection
/// / keyword / import session / format). An empty scope is the library-wide pass, byte-for-byte as
/// before. The scope is resolved to ids ONCE, up front, so the job is unaffected by later filter
/// changes — and before any model is loaded, so an empty scope costs nothing.
///
/// `stages` selects which stages run. `Objects`/`Animals`/`Faces`/`Captions` are honoured here;
/// `Panoramas` is a separate job and is ignored (the coordinator in `scan.rs` runs it). Passing the
/// full set reproduces the historical behaviour.
pub fn run_pass<R: Runtime>(
    app: &AppHandle<R>,
    force: bool,
    scope: &ScanScope,
    stages: &[StageId],
) -> Result<RunStats, String> {
    let st = app.state::<AppState>();
    if st.analysis_running.swap(true, Ordering::SeqCst) {
        return Err("analysis already running".into());
    }
    st.analysis_cancel.store(false, Ordering::SeqCst);
    let _guard = RunGuard {
        running: &st.analysis_running,
        cancel: &st.analysis_cancel,
    };

    // Resolve the scope before building anything expensive: an empty scope must not load ~280 MB of
    // models just to discover it has no work.
    let scope_ids: Option<Vec<i64>> = if scope.is_empty() {
        None
    } else {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        Some(scope_image_ids(&db.conn, scope).map_err(|e| e.to_string())?)
    };
    if scope_ids.as_ref().is_some_and(|ids| ids.is_empty()) {
        let stats = RunStats {
            analyzed: 0,
            failed: 0,
        };
        let _ = app.emit("analysis:done", &stats);
        return Ok(stats);
    }
    let scope_ids = scope_ids.as_deref();

    tracing::info!(
        accelerator = core_analyze::accelerator(),
        force,
        scoped = scope_ids.map(|ids| ids.len()),
        "AI analysis pass starting"
    );

    // Phase-A analyzers (object detection / animals / presence). The captioner is built lazily in
    // Phase B via `build_captioner`, so it is not part of this registry.
    let registry = registry(&st, stages)?;
    // `registry` already built only the selected detectors, so this is the filtered list — and
    // because `a_specs` below is derived from it and the batch loop indexes `t.stale[k]` against the
    // same enumeration, filtering here keeps every stale-mask index aligned by construction.
    let phase_a: Vec<&Arc<dyn Analyzer>> = registry.analyzers().iter().collect();

    // Faces participate when selected for THIS run AND models are present — never an implicit
    // download. The global `face_stage_enabled` setting seeds the UI's default; the per-run choice
    // is what actually gates the stage here.
    let face_on = stages.contains(&StageId::Faces) && faces_models_ready(&st);
    let fa = if face_on {
        Some(build_face_analyzer(&st)?)
    } else {
        None
    };

    // Phase-A dirty-DAG specs: object analyzers (in order) then the face_scan stage.
    let mut a_specs: Vec<StageSpec> = phase_a
        .iter()
        .map(|a| StageSpec {
            analyzer_id: a.id(),
            model_version: a.model_version(),
        })
        .collect();
    if face_on {
        a_specs.push(StageSpec {
            analyzer_id: FACE_ANALYZER_ID,
            model_version: FACE_MODEL_VERSION,
        });
    }
    let face_idx = face_on.then(|| a_specs.len() - 1);

    let mut analyzed = 0usize;
    let failed = AtomicUsize::new(0);

    // ---- Phase A: detection + faces ----
    // With nothing selected for Phase A (e.g. a captions-only run) `a_specs` is empty, and an empty
    // spec list would make `stale_targets` match nothing — so skip the loop rather than spin it.
    let total_a = if a_specs.is_empty() {
        0
    } else {
        pass_total(&st, &a_specs, scope_ids, force)?
    };
    emit_progress(app, "detect", 0, total_a);
    let done = AtomicUsize::new(0);
    let mut faces_added = false;
    let mut a_src = TargetSource::new(scope_ids);
    while !a_specs.is_empty() {
        if st.analysis_cancel.load(Ordering::SeqCst) {
            break;
        }
        let page = page_targets(&st, &a_specs, &mut a_src, ANALYSIS_BATCH, force)?;
        if page.is_empty() {
            break;
        }

        // Every VISITED photo yields an outcome — `map`, not `filter_map`. A photo that fails to
        // decode used to be dropped here, leaving no row anywhere: indistinguishable from "never
        // scanned", and silently re-failing on every future run. It now reaches the transaction
        // carrying an error attempt per stale stage.
        let results: Vec<PhotoOutcome> = page
            .par_iter()
            .map(|t| {
                let need_face = face_idx.map(|fi| t.stale[fi]).unwrap_or(false) && fa.is_some();
                // Stages this photo owes work for — the only ones an attempt may be recorded for.
                // Recording a skipped-because-clean stage would stamp an older timestamp over its
                // own newer `ok` row.
                let stale_specs = |t: &StaleTarget| -> Vec<(String, String)> {
                    a_specs
                        .iter()
                        .enumerate()
                        .filter(|(k, _)| t.stale[*k])
                        .map(|(_, s)| (s.analyzer_id.to_string(), s.model_version.to_string()))
                        .collect()
                };
                let bump = || {
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(32) {
                        emit_progress(app, "detect", done.load(Ordering::Relaxed), total_a);
                    }
                };

                let Some((native, oriented)) = decode_shared(&t.path, need_face) else {
                    failed.fetch_add(1, Ordering::Relaxed);
                    bump();
                    let msg = "decode failed";
                    tracing::warn!(image_id = t.id, path = %t.path, "{msg}");
                    return PhotoOutcome {
                        id: t.id,
                        inputs: Vec::new(),
                        face: None,
                        attempts: stale_specs(t)
                            .into_iter()
                            .map(|(stage_id, model_version)| OwnedAttempt {
                                stage_id,
                                model_version,
                                error: Some(msg.to_string()),
                            })
                            .collect(),
                    };
                };

                let mut records: Vec<core_analyze::AnalysisRecord> = Vec::new();
                let mut attempts: Vec<OwnedAttempt> = Vec::new();
                for (k, a) in phase_a.iter().enumerate() {
                    if !t.stale[k] {
                        continue;
                    }
                    let ctx = AnalysisCtx {
                        image_id: t.id,
                        content_hash_hex: &t.content_hash_hex,
                        image: &native,
                        prior: &records,
                    };
                    match a.analyze(&ctx) {
                        Ok(r) => {
                            attempts.push(OwnedAttempt {
                                stage_id: r.analyzer_id.clone(),
                                model_version: r.model_version.clone(),
                                error: None,
                            });
                            records.push(r);
                        }
                        Err(e) => {
                            tracing::warn!(image_id = t.id, analyzer = a.id(), error = %e, "analyzer failed");
                            attempts.push(OwnedAttempt {
                                stage_id: a.id().to_string(),
                                model_version: a.model_version().to_string(),
                                error: Some(e.to_string()),
                            });
                        }
                    }
                }
                // `Err` = face inference errored → no marker is written, so the image retries next
                // run (NOT a swallowed zero-face success). `None` = stage not needed/disabled here.
                let face_out: FaceOut = match (need_face, fa.as_ref(), oriented.as_ref()) {
                    (true, Some(f), Some(img)) => Some(match f.detect_embed(img) {
                        Ok(recs) => {
                            attempts.push(OwnedAttempt {
                                stage_id: FACE_ANALYZER_ID.to_string(),
                                model_version: FACE_MODEL_VERSION.to_string(),
                                error: None,
                            });
                            Ok(recs.into_iter().map(to_input).collect())
                        }
                        Err(e) => {
                            tracing::warn!(image_id = t.id, error = %e, "face analysis failed");
                            attempts.push(OwnedAttempt {
                                stage_id: FACE_ANALYZER_ID.to_string(),
                                model_version: FACE_MODEL_VERSION.to_string(),
                                error: Some(e.to_string()),
                            });
                            Err(e.to_string())
                        }
                    }),
                    _ => None,
                };
                let inputs: Vec<AnalysisInput> = records
                    .into_iter()
                    .map(|r| AnalysisInput {
                        analyzer_id: r.analyzer_id,
                        model_version: r.model_version,
                        payload: r.payload,
                    })
                    .collect();
                bump();
                PhotoOutcome {
                    id: t.id,
                    inputs,
                    face: face_out,
                    attempts,
                }
            })
            .collect();

        if !results.is_empty() {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let now = core_library::now_epoch();
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            for o in &results {
                if !o.inputs.is_empty() {
                    insert_analysis(&tx, o.id, now, &o.inputs).map_err(|e| e.to_string())?;
                }
                if let Some(Ok(faces)) = &o.face {
                    reconcile_faces(&tx, o.id, FACE_MODEL_VERSION, FACE_MODEL_TAG, now, faces)
                        .map_err(|e| e.to_string())?;
                    let marker = [AnalysisInput {
                        analyzer_id: FACE_ANALYZER_ID.to_string(),
                        model_version: FACE_MODEL_VERSION.to_string(),
                        payload: serde_json::json!({ "faces": faces.len() }),
                    }];
                    insert_analysis(&tx, o.id, now, &marker).map_err(|e| e.to_string())?;
                    if !faces.is_empty() {
                        faces_added = true;
                    }
                }
                record_attempts(&tx, o.id, now, &o.borrowed_attempts())
                    .map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
            // Count photos that produced at least one successful stage — decode failures are in
            // `results` now, and must not inflate "analyzed".
            analyzed += results.iter().filter(|o| o.produced_work()).count();
        }
        emit_progress(app, "detect", done.load(Ordering::Relaxed), total_a);
    }

    // Place the faces this run found — deliberately also on the cancel path, see `run_clustering`.
    if face_on {
        run_clustering(app, &st, faces_added)?;
    }

    // ---- Phase B: captions (deferred, non-blocking) ----
    // Build Florence ONLY when there's caption work, so a run with nothing to caption never loads
    // ~280 MB; the captioner drops at the end of this block (out of memory between/after scans).
    let b_specs = [StageSpec {
        analyzer_id: CAPTION_ID,
        model_version: CAPTION_VERSION,
    }];
    let captions_on = stages.contains(&StageId::Captions);
    let total_b = if captions_on {
        pass_total(&st, &b_specs, scope_ids, force)?
    } else {
        0
    };
    if total_b > 0 && !st.analysis_cancel.load(Ordering::SeqCst) {
        let cap = build_captioner(&st)?;
        emit_progress(app, "caption", 0, total_b);
        let bdone = AtomicUsize::new(0);
        let mut b_src = TargetSource::new(scope_ids);
        loop {
            if st.analysis_cancel.load(Ordering::SeqCst) {
                break;
            }
            let page = page_targets(&st, &b_specs, &mut b_src, ANALYSIS_BATCH, force)?;
            if page.is_empty() {
                break;
            }
            // As in Phase A: `map`, so a decode or captioner failure is recorded rather than
            // silently dropped. The caption stage is the only one in flight here, so an outcome
            // carries at most one attempt.
            let results: Vec<PhotoOutcome> = page
                .par_iter()
                .map(|t| {
                    let decoded = decode_shared(&t.path, false);
                    let outcome = match decoded {
                        None => Err("decode failed".to_string()),
                        Some((native, _)) => {
                            let ctx = AnalysisCtx {
                                image_id: t.id,
                                content_hash_hex: &t.content_hash_hex,
                                image: &native,
                                prior: &[],
                            };
                            match cap.analyze(&ctx) {
                                Ok(r) => Ok(AnalysisInput {
                                    analyzer_id: r.analyzer_id,
                                    model_version: r.model_version,
                                    payload: r.payload,
                                }),
                                Err(e) => {
                                    tracing::warn!(image_id = t.id, error = %e, "caption analysis failed");
                                    Err(e.to_string())
                                }
                            }
                        }
                    };
                    if outcome.is_err() {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                    let n = bdone.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(32) {
                        emit_progress(app, "caption", bdone.load(Ordering::Relaxed), total_b);
                    }
                    let attempt = OwnedAttempt {
                        stage_id: CAPTION_ID.to_string(),
                        model_version: CAPTION_VERSION.to_string(),
                        error: outcome.as_ref().err().cloned(),
                    };
                    PhotoOutcome {
                        id: t.id,
                        inputs: outcome.into_iter().collect(),
                        face: None,
                        attempts: vec![attempt],
                    }
                })
                .collect();
            if !results.is_empty() {
                let mut db = st.db.lock().map_err(|e| e.to_string())?;
                let now = core_library::now_epoch();
                let tx = db.conn.transaction().map_err(|e| e.to_string())?;
                for o in &results {
                    if !o.inputs.is_empty() {
                        insert_analysis(&tx, o.id, now, &o.inputs).map_err(|e| e.to_string())?;
                    }
                    record_attempts(&tx, o.id, now, &o.borrowed_attempts())
                        .map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())?;
                analyzed += results.iter().filter(|o| o.produced_work()).count();
            }
            emit_progress(app, "caption", bdone.load(Ordering::Relaxed), total_b);
        }
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let _ = db.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    }

    let stats = RunStats {
        analyzed,
        failed: failed.load(Ordering::Relaxed),
    };
    let _ = app.emit("analysis:done", &stats);
    Ok(stats)
}

/// Phase A→B boundary: place the faces this run detected, then a PASSIVE WAL checkpoint.
///
/// **Runs even when the scan was cancelled** (as long as this run actually produced faces). Faces
/// are durable the moment their batch commits, but a face with no `person_id` has no `person` row
/// and therefore appears in *no* navigation surface — so stopping without clustering left the user's
/// work invisible until some later scan happened to run.
///
/// Two things make that safe to do on the way out:
/// * The expensive `O(dirty × n)` scan runs with **no DB lock held** (snapshot → plan → apply), so
///   "Finalising faces…" never blocks the rest of the app.
/// * It watches `cluster_cancel`, **not** `analysis_cancel` — the latter is already `true` on this
///   path, so reusing it would make the pass abort instantly and silently do nothing. A second Stop
///   trips `cluster_cancel` and abandons the pass; anything unplaced stays durable and is reported
///   as "N ungrouped faces".
fn run_clustering<R: Runtime>(
    app: &AppHandle<R>,
    st: &AppState,
    faces_added: bool,
) -> Result<(), String> {
    let cancelled = st.analysis_cancel.load(Ordering::SeqCst);
    // On the cancel path only finish what THIS run started; a full sweep of pre-existing dirty faces
    // is the next scan's job and would make Stop arbitrarily long for no user-visible gain.
    let should_run = if cancelled {
        faces_added
    } else {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        faces_added || has_dirty_faces(&db.conn, FACE_MODEL_TAG).map_err(|e| e.to_string())?
    };

    if should_run {
        st.cluster_cancel.store(false, Ordering::SeqCst);
        // 1. Snapshot — short lock.
        let snap = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            cluster_snapshot(&db.conn, FACE_MODEL_TAG).map_err(|e| e.to_string())?
        };
        // 2. Plan — no lock held.
        emit_progress(app, "finalize", 0, 0);
        let plan = plan_clusters(
            &snap,
            ClusterParams::default(),
            &st.cluster_cancel,
            &mut |done, total| emit_progress(app, "finalize", done, total as i64),
        );
        // 3. Apply — short lock, flat keyed UPDATEs.
        {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let now = core_library::now_epoch();
            apply_cluster_plan(&mut db.conn, now, &plan).map_err(|e| e.to_string())?;
        }
    }

    // Checkpoint on every path, cancelled included — previously this sat after an early return, so
    // a cancelled run never checkpointed at all.
    let db = st.db.lock().map_err(|e| e.to_string())?;
    let _ = db.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
}

/// The `(analyzer_id, model_version)` a stage is currently gated on, derived from constants instead
/// of from a built [`registry`] — so the modal can price its work without loading ~280 MB of models.
///
/// These cannot drift from the real pass: `registry` constructs each analyzer with exactly these
/// id/version constants (`ObjectDetector::new(.., DETECTOR_VERSION)` etc.), and each `Analyzer::id`
/// returns the same `core_analyze` constant. Panorama is version-gated on its own `ALGO_VERSION`.
pub fn stage_spec(st: &AppState, stage: StageId) -> Result<StageSpec, String> {
    Ok(match stage {
        StageId::Objects => StageSpec {
            analyzer_id: core_analyze::OBJECT_DETECTION_ID,
            model_version: DETECTOR_VERSION,
        },
        StageId::Animals => {
            let an_size = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::animal_detector_size(&db.conn).map_err(|e| e.to_string())?
            };
            StageSpec {
                analyzer_id: core_analyze::ANIMAL_DETECTION_ID,
                model_version: if an_size == 640 {
                    ANIMAL_DETECTOR_VERSION_640
                } else {
                    ANIMAL_DETECTOR_VERSION_1280
                },
            }
        }
        StageId::Faces => StageSpec {
            analyzer_id: FACE_ANALYZER_ID,
            model_version: FACE_MODEL_VERSION,
        },
        StageId::Captions => StageSpec {
            analyzer_id: core_analyze::CAPTION_ID,
            model_version: CAPTION_VERSION,
        },
        StageId::Panoramas => StageSpec {
            analyzer_id: core_library::PANORAMA_STAGE_ID,
            model_version: core_library::pano_detect::ALGO_VERSION,
        },
    })
}

/// How much work each stage has inside `scope` — sizes the scan modal's rows.
///
/// Cheap by construction: one scope-id query plus one count per stage, no model loading, so it is
/// safe to re-run on every filter change. Counts only *pending* work (the same per-stage dirty-DAG
/// the pass itself uses), so a fully-scanned folder reads 0.
pub fn scope_counts(st: &AppState, scope: &ScanScope) -> Result<ScopeCounts, String> {
    let ids = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        scope_image_ids(&db.conn, scope).map_err(|e| e.to_string())?
    };
    let mut stages = Vec::with_capacity(StageId::ALL.len());
    for stage in StageId::ALL {
        let ready = stage_models_ready(st, stage);
        // A stage whose models are missing reports `None`, not 0 — "nothing to do" and "can't run"
        // must not look the same in the UI.
        let pending = if !ready {
            None
        } else if stage == StageId::Panoramas {
            // Panorama detection has no scoped entry point, so this figure is always library-wide.
            // It uses the scan's own >=2-frame cluster eligibility, or lone photos would show as
            // permanently pending.
            let db = st.db.lock().map_err(|e| e.to_string())?;
            Some(
                core_library::pano_detect::pending_count(
                    &db.conn,
                    core_library::pano_detect::ALGO_VERSION,
                )
                .map_err(|e| e.to_string())?,
            )
        } else {
            let spec = stage_spec(st, stage)?;
            let db = st.db.lock().map_err(|e| e.to_string())?;
            Some(stale_count_in(&db.conn, &[spec], &ids).map_err(|e| e.to_string())?)
        };
        stages.push(core_library::StagePending {
            stage,
            pending,
            models_ready: ready,
            library_wide: stage == StageId::Panoramas,
        });
    }
    Ok(ScopeCounts {
        total: ids.len() as i64,
        stages,
    })
}

/// The per-photo scan record: which stages have run, when, and which failed.
pub fn image_scan_state(
    st: &AppState,
    image_id: i64,
) -> Result<core_library::ImageScanState, String> {
    let specs: Vec<(StageId, StageSpec)> = StageId::ALL
        .iter()
        .map(|s| stage_spec(st, *s).map(|spec| (*s, spec)))
        .collect::<Result<_, String>>()?;
    let pairs: Vec<(&str, &str)> = specs
        .iter()
        .map(|(_, spec)| (spec.analyzer_id, spec.model_version))
        .collect();
    let db = st.db.lock().map_err(|e| e.to_string())?;
    core_library::image_scan_state(&db.conn, image_id, &pairs).map_err(|e| e.to_string())
}

/// Status for the UI: total present images, how many have BOTH analyzers at the current version.
pub fn status(st: &AppState) -> Result<AnalysisStatus, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    let total = core_library::present_image_count(&db.conn).map_err(|e| e.to_string())?;
    let seen = existing_analysis(&db.conn).map_err(|e| e.to_string())?;
    let targets = present_images(&db.conn).map_err(|e| e.to_string())?;
    drop(db);
    let analyzed = targets
        .iter()
        .filter(|t| {
            seen.contains(&(t.id, "object_detection".into(), DETECTOR_VERSION.into()))
                && seen.contains(&(t.id, "caption".into(), CAPTION_VERSION.into()))
        })
        .count() as i64;
    Ok(AnalysisStatus {
        total,
        analyzed,
        pending: total - analyzed,
        models_ready: models_ready(st),
        running: st.analysis_running.load(Ordering::SeqCst),
        accelerator: core_analyze::accelerator().to_string(),
    })
}
