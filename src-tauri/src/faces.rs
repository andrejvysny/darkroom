//! Manual "Find People" face pass: decode → SCRFD detect → align → ArcFace embed → store → cluster.
//!
//! A dedicated pass (NOT part of the auto object-detection analysis) so it runs only on demand and
//! never re-runs the expensive captioner. Mirrors `analysis.rs` discipline: parallel unlocked
//! decode+infer in batches, each committed in a short transaction, then one incremental clustering
//! pass. Per-image completion is marked with a `face_detection` row in `analysis_results`, reusing the
//! existing version-gated incremental skip (so an image with zero faces is still recorded as done).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use core_analyze::face::DEFAULT_FACE_DET_EDGE;
use core_analyze::models::{ModelStore, FACE_DETECTOR_FILES, FACE_EMBEDDER_FILES};
use core_analyze::{FaceAnalyzer, FaceRecord};
use core_library::{
    cluster_assign, existing_analysis, insert_analysis, insert_faces, present_images,
    AnalysisInput, ClusterParams, ClusterStats, FaceInput,
};
use image::imageops::FilterType;
use rayon::prelude::*;
use serde::Serialize;
use std::path::Path;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Longest edge the face-pass decode is downscaled to. Larger than the analysis decode (1024) for
/// sharper alignment crops; SCRFD still letterboxes to its own fixed input.
const FACE_DECODE_EDGE: u32 = 1536;

/// Decode an image's embedded preview to sRGB **uprighted to EXIF orientation** (so face boxes line
/// up with the displayed thumbnail), downscaled so the longest edge ≤ [`FACE_DECODE_EDGE`].
fn decode_oriented(path: &str) -> Option<image::RgbImage> {
    let src = core_raw::source_from_path(Path::new(path)).ok()?;
    let img = core_raw::oriented_preview(&src).ok()?.to_rgb8();
    let m = img.width().max(img.height());
    if m > FACE_DECODE_EDGE {
        let s = FACE_DECODE_EDGE as f32 / m as f32;
        Some(image::imageops::resize(
            &img,
            (img.width() as f32 * s) as u32,
            (img.height() as f32 * s) as u32,
            FilterType::Triangle,
        ))
    } else {
        Some(img)
    }
}

/// Marker analyzer id + version stored in `analysis_results` to gate incremental face re-processing.
pub const FACE_ANALYZER_ID: &str = "face_detection";
pub const FACE_MODEL_VERSION: &str = "scrfd10g+arcface_w600k_r50_v1";
/// Embedding tag on `face_embedding.model_tag`; a change invalidates vectors → re-embed + re-cluster.
pub const FACE_MODEL_TAG: &str = "scrfd10g+arcface_w600k_r50_v1";

const FACE_BATCH: usize = 8;

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

/// Build (once) and cache the face analyzer (SCRFD + ArcFace, ~190 MB ONNX). Errors if not downloaded.
fn analyzer(st: &AppState) -> Result<Arc<FaceAnalyzer>, String> {
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

/// Resets the running/cancel flags on drop so an early return can't wedge the guard.
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

fn to_input(r: FaceRecord) -> FaceInput {
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

/// Run the face pass. `force` re-processes every image. Emits `faces:progress`/`faces:done`.
pub fn run_pass<R: Runtime>(app: &AppHandle<R>, force: bool) -> Result<FacesRunStats, String> {
    let st = app.state::<AppState>();
    if st.faces_running.swap(true, Ordering::SeqCst) {
        return Err("face pass already running".into());
    }
    st.faces_cancel.store(false, Ordering::SeqCst);
    let _guard = RunGuard {
        running: &st.faces_running,
        cancel: &st.faces_cancel,
    };

    let fa = analyzer(&st)?;

    let (targets, seen) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let targets = present_images(&db.conn).map_err(|e| e.to_string())?;
        let seen = existing_analysis(&db.conn).map_err(|e| e.to_string())?;
        (targets, seen)
    };
    let todo: Vec<_> = targets
        .into_iter()
        .filter(|t| {
            force
                || !seen.contains(&(
                    t.id,
                    FACE_ANALYZER_ID.to_string(),
                    FACE_MODEL_VERSION.to_string(),
                ))
        })
        .collect();

    let total = todo.len();
    let _ = app.emit(
        "faces:progress",
        serde_json::json!({ "done": 0, "total": total }),
    );
    let done = AtomicUsize::new(0);
    let mut images_done = 0usize;
    let mut faces_total = 0usize;

    for batch in todo.chunks(FACE_BATCH) {
        if st.faces_cancel.load(Ordering::SeqCst) {
            break;
        }
        // Decode + detect + embed in parallel (no DB lock). Keep zero-face images so they get a
        // completion marker and aren't re-processed.
        let results: Vec<(i64, Vec<FaceInput>)> = batch
            .par_iter()
            .filter_map(|t| {
                let out = decode_oriented(&t.path).map(|img| {
                    let faces = match fa.detect_embed(&img) {
                        Ok(recs) => recs.into_iter().map(to_input).collect(),
                        Err(e) => {
                            eprintln!("[darkroom] face analyze failed on {}: {e}", t.path);
                            Vec::new()
                        }
                    };
                    (t.id, faces)
                });
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n == total || n.is_multiple_of(2) {
                    let _ = app.emit(
                        "faces:progress",
                        serde_json::json!({ "done": n, "total": total }),
                    );
                }
                out
            })
            .collect();

        if !results.is_empty() {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let now = core_library::now_epoch();
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            for (id, faces) in &results {
                insert_faces(&tx, *id, FACE_MODEL_VERSION, FACE_MODEL_TAG, now, faces)
                    .map_err(|e| e.to_string())?;
                // Completion marker (canonical row only; no projection for this analyzer id).
                let marker = [AnalysisInput {
                    analyzer_id: FACE_ANALYZER_ID.to_string(),
                    model_version: FACE_MODEL_VERSION.to_string(),
                    payload: serde_json::json!({ "faces": faces.len() }),
                }];
                insert_analysis(&tx, *id, now, &marker).map_err(|e| e.to_string())?;
                faces_total += faces.len();
            }
            tx.commit().map_err(|e| e.to_string())?;
            images_done += results.len();
        }
        let _ = app.emit(
            "faces:progress",
            serde_json::json!({ "done": done.load(Ordering::Relaxed), "total": total }),
        );
    }

    // One incremental clustering pass over all embedded faces at this model tag.
    let cluster = {
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let now = core_library::now_epoch();
        cluster_assign(&mut db.conn, FACE_MODEL_TAG, now, ClusterParams::default())
            .map_err(|e| e.to_string())?
    };

    let stats = FacesRunStats {
        images: images_done,
        faces: faces_total,
        cluster,
    };
    let _ = app.emit("faces:done", &stats);
    Ok(stats)
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
        running: st.faces_running.load(Ordering::SeqCst),
        faces,
        people,
    })
}
