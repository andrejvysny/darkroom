//! Face models + People status + the "Find People" trigger.
//!
//! Face detection/embedding/clustering now runs as a STAGE of the unified scan (`crate::analysis`),
//! not a separate pass. This module keeps the face MODEL plumbing (download + lazy build), the
//! per-image marker constants, the `FaceRecord → FaceInput` adapter, the People status projection, and
//! a thin `run_pass` shim that delegates to `crate::analysis::run_pass` — so "Find People" and
//! "Analyze" trigger one and the same scan, sharing a single decode + run-guard.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use core_analyze::face::DEFAULT_FACE_DET_EDGE;
use core_analyze::models::{ModelStore, FACE_DETECTOR_FILES, FACE_EMBEDDER_FILES};
use core_analyze::{FaceAnalyzer, FaceRecord};
use core_library::{existing_analysis, present_images, ClusterStats, FaceInput};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Longest edge the oriented (face) view is downscaled to — larger than the object view (1024) for
/// sharper alignment crops; SCRFD still letterboxes to its own fixed input. Used by the unified pass.
pub(crate) const FACE_DECODE_EDGE: u32 = 1536;

/// Marker analyzer id + version stored in `analysis_results` to gate incremental face re-processing.
pub const FACE_ANALYZER_ID: &str = "face_detection";
pub const FACE_MODEL_VERSION: &str = "scrfd10g+arcface_w600k_r50_v1";
/// Embedding tag on `face_embedding.model_tag`; a change invalidates vectors → re-embed + re-cluster.
pub const FACE_MODEL_TAG: &str = "scrfd10g+arcface_w600k_r50_v1";

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacesStatus {
    pub total: i64,
    pub processed: i64,
    pub pending: i64,
    pub models_ready: bool,
    pub running: bool,
    pub faces: i64,
    pub people: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacesRunStats {
    pub images: usize,
    pub faces: usize,
    pub cluster: ClusterStats,
}

/// True once both face model files are present.
pub fn faces_models_ready(st: &AppState) -> bool {
    let store = ModelStore::new(st.models_dir.clone());
    store.has_all(FACE_DETECTOR_FILES) && store.has_all(FACE_EMBEDDER_FILES)
}

/// Download any missing face model files (~190 MB on first run). Emits `faces:models` `{done,total}`.
pub fn ensure_face_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let store = ModelStore::new(st.models_dir.clone());
    let total = FACE_DETECTOR_FILES.len() + FACE_EMBEDDER_FILES.len();
    let emit = |done: usize| {
        let _ = app.emit(
            "faces:models",
            serde_json::json!({ "done": done, "total": total }),
        );
    };
    emit(0);
    store
        .ensure(FACE_DETECTOR_FILES, |i, _| emit(i))
        .map_err(|e| e.to_string())?;
    let off = FACE_DETECTOR_FILES.len();
    store
        .ensure(FACE_EMBEDDER_FILES, |i, _| emit(off + i))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Build (once) and cache the face analyzer (SCRFD + ArcFace, ~190 MB ONNX). `pub(crate)` so the
/// unified pass owns the face stage. Errors if the models aren't downloaded.
pub(crate) fn analyzer(st: &AppState) -> Result<Arc<FaceAnalyzer>, String> {
    if let Some(a) = st.face_analyzer.lock().map_err(|e| e.to_string())?.as_ref() {
        return Ok(a.clone());
    }
    if !faces_models_ready(st) {
        return Err("face models not downloaded".into());
    }
    let store = ModelStore::new(st.models_dir.clone());
    let fa = FaceAnalyzer::new(
        &store.face_detector_path(),
        &store.face_embedder_path(),
        DEFAULT_FACE_DET_EDGE,
    )
    .map_err(|e| e.to_string())?;
    let arc = Arc::new(fa);
    *st.face_analyzer.lock().map_err(|e| e.to_string())? = Some(arc.clone());
    Ok(arc)
}

/// `FaceRecord` (ML crate) → `FaceInput` (catalog). `pub(crate)` for the unified pass's face stage.
pub(crate) fn to_input(r: FaceRecord) -> FaceInput {
    let mut kps = [0f32; 10];
    for (i, p) in r.kps.iter().enumerate() {
        kps[2 * i] = p[0];
        kps[2 * i + 1] = p[1];
    }
    FaceInput {
        bbox: r.bbox,
        kps,
        det_score: r.det_score,
        quality: r.quality,
        embedding: r.embedding,
    }
}

/// "Find People": delegates to the unified scan (faces run as a gated stage when enabled + models
/// present). Kept as a shim so the existing `faces_run` IPC + People UI keep working unchanged; the
/// real per-run face counts surface via `faces:done` + [`status`].
pub fn run_pass<R: Runtime>(app: &AppHandle<R>, force: bool) -> Result<FacesRunStats, String> {
    let stats = crate::analysis::run_pass(app, force)?;
    Ok(FacesRunStats {
        images: stats.analyzed,
        faces: 0,
        cluster: ClusterStats::default(),
    })
}

/// People status for the UI: present-image total, how many have been face-processed, and counts.
pub fn status(st: &AppState) -> Result<FacesStatus, String> {
    let (total, processed, faces, people) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let total = core_library::present_image_count(&db.conn).map_err(|e| e.to_string())?;
        let seen = existing_analysis(&db.conn).map_err(|e| e.to_string())?;
        let targets = present_images(&db.conn).map_err(|e| e.to_string())?;
        let (faces, people) = core_library::faces_summary(&db.conn).map_err(|e| e.to_string())?;
        let processed = targets
            .iter()
            .filter(|t| {
                seen.contains(&(
                    t.id,
                    FACE_ANALYZER_ID.to_string(),
                    FACE_MODEL_VERSION.to_string(),
                ))
            })
            .count() as i64;
        (total, processed, faces, people)
    };
    Ok(FacesStatus {
        total,
        processed,
        pending: (total - processed).max(0),
        models_ready: faces_models_ready(st),
        // Faces now run inside the unified scan, guarded by `analysis_running`.
        running: st.analysis_running.load(Ordering::SeqCst),
        faces,
        people,
    })
}
