//! Background AI analysis pass: decode → run analyzers (rayon, no DB lock) → bulk-insert results.
//!
//! Lives here (not in `core-library`) because it bridges the ML crate (`core-analyze`) and the
//! catalog (`core-library`), keeping `core-library` free of any ONNX/ort dependency. Mirrors the
//! indexing pass's discipline: parallel unlocked work, then one brief locked transaction.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use core_analyze::models::{ModelStore, CAPTION_FILES, DETECTOR_FILES};
use core_analyze::{AnalysisCtx, AnalyzerRegistry, Captioner, ObjectDetector};
use core_library::{existing_analysis, insert_analysis, present_images, AnalysisInput};
use image::imageops::FilterType;
use rayon::prelude::*;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Model-version tags stored per result row; bump to force re-analysis of all images.
pub const DETECTOR_VERSION: &str = "dfine-m-coco-v1";
pub const CAPTION_VERSION: &str = "florence2-base-ft-q4f16-v1";

/// Longest-edge the analysis decode is downscaled to (boxes are normalized, so this is loss-only).
const ANALYZE_EDGE: u32 = 1024;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisStatus {
    pub total: i64,
    pub analyzed: i64,
    pub pending: i64,
    pub models_ready: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStats {
    pub analyzed: usize,
    pub failed: usize,
}

/// True once every detector + caption model file is present.
pub fn models_ready(st: &AppState) -> bool {
    let store = ModelStore::new(st.models_dir.clone());
    store.has_all(DETECTOR_FILES) && store.has_all(CAPTION_FILES)
}

/// Download any missing model files, emitting `analysis:models` `{done,total}` progress.
pub fn ensure_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let store = ModelStore::new(st.models_dir.clone());
    let total = DETECTOR_FILES.len() + CAPTION_FILES.len();
    let emit = |done: usize| {
        let _ = app.emit(
            "analysis:models",
            serde_json::json!({ "done": done, "total": total }),
        );
    };
    emit(0);
    let offset = DETECTOR_FILES.len();
    store
        .ensure(DETECTOR_FILES, |i, _| emit(i))
        .map_err(|e| e.to_string())?;
    store
        .ensure(CAPTION_FILES, |i, _| emit(offset + i))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Build (once) and cache the analyzer registry. Errors if models aren't downloaded yet.
fn registry(st: &AppState) -> Result<Arc<AnalyzerRegistry>, String> {
    if let Some(r) = st.analyzers.lock().map_err(|e| e.to_string())?.as_ref() {
        return Ok(r.clone());
    }
    if !models_ready(st) {
        return Err("models not downloaded".into());
    }
    let store = ModelStore::new(st.models_dir.clone());
    let florence = store.florence_dir();
    let mut reg = AnalyzerRegistry::new();
    reg.register(Arc::new(
        ObjectDetector::new(&store.detector_path(), DETECTOR_VERSION).map_err(|e| e.to_string())?,
    ));
    reg.register(Arc::new(
        Captioner::new(&florence, &florence.join("tokenizer.json"), CAPTION_VERSION)
            .map_err(|e| e.to_string())?,
    ));
    let arc = Arc::new(reg);
    *st.analyzers.lock().map_err(|e| e.to_string())? = Some(arc.clone());
    Ok(arc)
}

/// Decode an image's embedded preview to sRGB and downscale to <= ANALYZE_EDGE (longest edge).
fn decode_srgb(path: &str) -> Option<image::RgbImage> {
    let src = core_raw::source_from_path(Path::new(path)).ok()?;
    let img = core_raw::preview_image(&src).ok()?.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let m = w.max(h);
    if m > ANALYZE_EDGE {
        let s = ANALYZE_EDGE as f32 / m as f32;
        Some(image::imageops::resize(
            &img,
            (w as f32 * s) as u32,
            (h as f32 * s) as u32,
            FilterType::Triangle,
        ))
    } else {
        Some(img)
    }
}

/// Resets the `analysis_running` flag on drop (so an early return / error can't wedge the guard).
struct RunGuard<'a>(&'a AtomicBool);
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Run the analysis pass. `force` re-analyzes every image; otherwise only images missing any
/// enabled analyzer@version. Emits `analysis:progress` `{done,total}` and `analysis:done`.
pub fn run_pass<R: Runtime>(app: &AppHandle<R>, force: bool) -> Result<RunStats, String> {
    let st = app.state::<AppState>();
    if st.analysis_running.swap(true, Ordering::SeqCst) {
        return Err("analysis already running".into());
    }
    let _guard = RunGuard(&st.analysis_running);

    let registry = registry(&st)?;
    let analyzers = registry.analyzers();

    // Snapshot targets + already-analyzed triples under a brief lock.
    let (targets, seen) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let targets = present_images(&db.conn).map_err(|e| e.to_string())?;
        let seen = existing_analysis(&db.conn).map_err(|e| e.to_string())?;
        (targets, seen)
    };

    // An image is to-do if it's missing ANY enabled analyzer@version (then we recompute ALL of them,
    // so the caption stage always sees fresh detection results via `prior`).
    let todo: Vec<_> = targets
        .into_iter()
        .filter(|t| {
            force
                || analyzers.iter().any(|a| {
                    !seen.contains(&(t.id, a.id().to_string(), a.model_version().to_string()))
                })
        })
        .collect();

    let total = todo.len();
    let _ = app.emit(
        "analysis:progress",
        serde_json::json!({ "done": 0, "total": total }),
    );
    let done = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);

    // Parallel decode + inference (inference serializes on each analyzer's internal mutex). No DB lock.
    let results: Vec<(i64, Vec<AnalysisInput>)> = todo
        .par_iter()
        .filter_map(|t| {
            let out = decode_srgb(&t.path).map(|img| {
                let mut records = Vec::new();
                for a in analyzers {
                    let ctx = AnalysisCtx {
                        image_id: t.id,
                        content_hash_hex: &t.content_hash_hex,
                        image: &img,
                        prior: &records,
                    };
                    match a.analyze(&ctx) {
                        Ok(rec) => records.push(rec),
                        Err(e) => {
                            eprintln!("[darkroom] analyzer {} failed on {}: {e}", a.id(), t.path)
                        }
                    }
                }
                let inputs: Vec<AnalysisInput> = records
                    .into_iter()
                    .map(|r| AnalysisInput {
                        analyzer_id: r.analyzer_id,
                        model_version: r.model_version,
                        payload: r.payload,
                    })
                    .collect();
                (t.id, inputs)
            });
            if out.is_none() {
                failed.fetch_add(1, Ordering::Relaxed);
            }
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n == total || n.is_multiple_of(2) {
                let _ = app.emit(
                    "analysis:progress",
                    serde_json::json!({ "done": n, "total": total }),
                );
            }
            out.filter(|(_, inputs)| !inputs.is_empty())
        })
        .collect();

    // One brief lock: bulk-insert all results in a single transaction.
    let analyzed = results.len();
    {
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let ran_at = core_library::now_epoch();
        let tx = db.conn.transaction().map_err(|e| e.to_string())?;
        for (id, inputs) in &results {
            insert_analysis(&tx, *id, ran_at, inputs).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }

    let stats = RunStats {
        analyzed,
        failed: failed.load(Ordering::Relaxed),
    };
    let _ = app.emit("analysis:done", &stats);
    Ok(stats)
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
    })
}
