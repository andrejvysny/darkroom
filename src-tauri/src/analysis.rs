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
    cluster_assign, existing_analysis, face_stage_enabled, has_dirty_faces, insert_analysis,
    present_image_count, present_images, present_targets_after, reconcile_faces, stale_count,
    stale_targets, AnalysisInput, ClusterParams, FaceInput, StageSpec, StaleTarget,
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

/// Download any missing model files, emitting `analysis:models` `{done,total}` progress.
pub fn ensure_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let store = ModelStore::new(st.models_dir.clone());
    let total = DETECTOR_FILES.len()
        + ANIMAL_DETECTOR_FILES.len()
        + CAPTION_FILES.len()
        + VERIFIER_FILES.len();
    let emit = |done: usize| {
        let _ = app.emit(
            "analysis:models",
            serde_json::json!({ "done": done, "total": total }),
        );
    };
    emit(0);
    store
        .ensure(DETECTOR_FILES, |i, _| emit(i))
        .map_err(|e| e.to_string())?;
    let off1 = DETECTOR_FILES.len();
    store
        .ensure(ANIMAL_DETECTOR_FILES, |i, _| emit(off1 + i))
        .map_err(|e| e.to_string())?;
    let off2 = off1 + ANIMAL_DETECTOR_FILES.len();
    store
        .ensure(CAPTION_FILES, |i, _| emit(off2 + i))
        .map_err(|e| e.to_string())?;
    let off3 = off2 + CAPTION_FILES.len();
    store
        .ensure(VERIFIER_FILES, |i, _| emit(off3 + i))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Build (once) and cache the **Phase-A** analyzer registry (object detection + animals + presence).
/// The captioner (Florence-2, ~280 MB) is NOT here — it's built lazily in Phase B via
/// [`build_captioner`] so it never sits in memory during the detection+faces phase. Errors if models
/// aren't downloaded yet.
fn registry(st: &AppState) -> Result<Arc<AnalyzerRegistry>, String> {
    if let Some(r) = st.analyzers.lock().map_err(|e| e.to_string())?.as_ref() {
        return Ok(r.clone());
    }
    if !models_ready(st) {
        return Err("models not downloaded".into());
    }
    let store = ModelStore::new(st.models_dir.clone());
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
    let mut reg = AnalyzerRegistry::new();
    reg.register(Arc::new(
        ObjectDetector::new(&store.detector_path(), DETECTOR_VERSION)
            .map_err(|e| e.to_string())?
            .with_verifier(verifier.clone()),
    ));
    reg.register(Arc::new(
        MegaDetector::new(&store.animal_detector_path(), an_ver, an_size)
            .map_err(|e| e.to_string())?
            .with_verifier(verifier.clone()),
    ));
    // Full-image linear-probe presence classifier — reuses the already-built CLIP verifier (vision
    // encoder), so no extra model load. Catches subjects the box detectors miss; fused at query time.
    reg.register(Arc::new(
        PresenceProbe::new(verifier.clone(), PRESENCE_VERSION).map_err(|e| e.to_string())?,
    ));
    let arc = Arc::new(reg);
    *st.analyzers.lock().map_err(|e| e.to_string())? = Some(arc.clone());
    Ok(arc)
}

/// Build the captioner (Florence-2, ~280 MB / 5 ONNX sessions) on demand for the deferred Phase B.
/// Built fresh per run and dropped when the caller's `Arc` falls out of scope, so Florence is resident
/// ONLY during captioning — never during the detection+faces phase or between scans.
fn build_captioner(st: &AppState) -> Result<Arc<Captioner>, String> {
    if !models_ready(st) {
        return Err("models not downloaded".into());
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

/// One keyset page of work for `phase`'s stage specs. `force` re-scans every present image (all stages
/// stale); otherwise only images with ≥1 stale stage. Never uses OFFSET — the caller advances the
/// cursor to the last returned id, so the shrinking dirty set never skips or repeats a row.
fn page_targets(
    st: &AppState,
    specs: &[StageSpec],
    cursor: i64,
    limit: i64,
    force: bool,
) -> Result<Vec<StaleTarget>, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    if force {
        let n = specs.len();
        Ok(present_targets_after(&db.conn, cursor, limit)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|t| StaleTarget {
                id: t.id,
                path: t.path,
                content_hash_hex: t.content_hash_hex,
                stale: vec![true; n],
            })
            .collect())
    } else {
        stale_targets(&db.conn, specs, cursor, limit).map_err(|e| e.to_string())
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
pub fn run_pass<R: Runtime>(app: &AppHandle<R>, force: bool) -> Result<RunStats, String> {
    let st = app.state::<AppState>();
    if st.analysis_running.swap(true, Ordering::SeqCst) {
        return Err("analysis already running".into());
    }
    st.analysis_cancel.store(false, Ordering::SeqCst);
    let _guard = RunGuard {
        running: &st.analysis_running,
        cancel: &st.analysis_cancel,
    };
    tracing::info!(
        accelerator = core_analyze::accelerator(),
        force,
        "AI analysis pass starting"
    );

    // Phase-A analyzers (object detection / animals / presence). The captioner is built lazily in
    // Phase B via `build_captioner`, so it is not part of this registry.
    let registry = registry(&st)?;
    let phase_a: Vec<&Arc<dyn Analyzer>> = registry.analyzers().iter().collect();

    // Faces participate when enabled (default on) AND models are present — never an implicit download.
    let face_on = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        face_stage_enabled(&db.conn).map_err(|e| e.to_string())?
    } && faces_models_ready(&st);
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
    let total_a = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        if force {
            present_image_count(&db.conn).map_err(|e| e.to_string())?
        } else {
            stale_count(&db.conn, &a_specs).map_err(|e| e.to_string())?
        }
    };
    emit_progress(app, "detect", 0, total_a);
    let done = AtomicUsize::new(0);
    let mut faces_added = false;
    let mut cursor = 0i64;
    loop {
        if st.analysis_cancel.load(Ordering::SeqCst) {
            break;
        }
        let page = page_targets(&st, &a_specs, cursor, ANALYSIS_BATCH as i64, force)?;
        let Some(last) = page.last().map(|t| t.id) else {
            break;
        };
        cursor = last;

        // `Err(())` = face inference errored → no marker is written, so the image retries next run
        // (NOT a swallowed zero-face success). `None` = face stage not needed/disabled for this image.
        type FaceOut = Option<Result<Vec<FaceInput>, ()>>;
        let results: Vec<(i64, Vec<AnalysisInput>, FaceOut)> = page
            .par_iter()
            .filter_map(|t| {
                let need_face = face_idx.map(|fi| t.stale[fi]).unwrap_or(false) && fa.is_some();
                let Some((native, oriented)) = decode_shared(&t.path, need_face) else {
                    failed.fetch_add(1, Ordering::Relaxed);
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(32) {
                        emit_progress(app, "detect", done.load(Ordering::Relaxed), total_a);
                    }
                    return None;
                };
                let mut records: Vec<core_analyze::AnalysisRecord> = Vec::new();
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
                        Ok(r) => records.push(r),
                        Err(e) => tracing::warn!(image_id = t.id, analyzer = a.id(), error = %e, "analyzer failed"),
                    }
                }
                let face_out: FaceOut = match (need_face, fa.as_ref(), oriented.as_ref()) {
                    (true, Some(f), Some(img)) => Some(match f.detect_embed(img) {
                        Ok(recs) => Ok(recs.into_iter().map(to_input).collect()),
                        Err(e) => {
                            tracing::warn!(image_id = t.id, error = %e, "face analysis failed");
                            Err(())
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
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(32) {
                    emit_progress(app, "detect", done.load(Ordering::Relaxed), total_a);
                }
                Some((t.id, inputs, face_out))
            })
            .collect();

        if !results.is_empty() {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let now = core_library::now_epoch();
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            for (id, inputs, face_out) in &results {
                if !inputs.is_empty() {
                    insert_analysis(&tx, *id, now, inputs).map_err(|e| e.to_string())?;
                }
                if let Some(Ok(faces)) = face_out {
                    reconcile_faces(&tx, *id, FACE_MODEL_VERSION, FACE_MODEL_TAG, now, faces)
                        .map_err(|e| e.to_string())?;
                    let marker = [AnalysisInput {
                        analyzer_id: FACE_ANALYZER_ID.to_string(),
                        model_version: FACE_MODEL_VERSION.to_string(),
                        payload: serde_json::json!({ "faces": faces.len() }),
                    }];
                    insert_analysis(&tx, *id, now, &marker).map_err(|e| e.to_string())?;
                    if !faces.is_empty() {
                        faces_added = true;
                    }
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
            analyzed += results.len();
        }
        emit_progress(app, "detect", done.load(Ordering::Relaxed), total_a);
    }

    // Cluster faces added/dirtied this run (skipped when nothing needs placing).
    if face_on {
        run_clustering(&st, faces_added)?;
    }

    // ---- Phase B: captions (deferred, non-blocking) ----
    // Build Florence ONLY when there's caption work, so a run with nothing to caption never loads
    // ~280 MB; the captioner drops at the end of this block (out of memory between/after scans).
    let b_specs = [StageSpec {
        analyzer_id: CAPTION_ID,
        model_version: CAPTION_VERSION,
    }];
    let total_b = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        if force {
            present_image_count(&db.conn).map_err(|e| e.to_string())?
        } else {
            stale_count(&db.conn, &b_specs).map_err(|e| e.to_string())?
        }
    };
    if total_b > 0 && !st.analysis_cancel.load(Ordering::SeqCst) {
        let cap = build_captioner(&st)?;
        emit_progress(app, "caption", 0, total_b);
        let bdone = AtomicUsize::new(0);
        let mut bcursor = 0i64;
        loop {
            if st.analysis_cancel.load(Ordering::SeqCst) {
                break;
            }
            let page = page_targets(&st, &b_specs, bcursor, ANALYSIS_BATCH as i64, force)?;
            let Some(last) = page.last().map(|t| t.id) else {
                break;
            };
            bcursor = last;
            let results: Vec<(i64, Vec<AnalysisInput>)> = page
                .par_iter()
                .filter_map(|t| {
                    let res = decode_shared(&t.path, false).and_then(|(native, _)| {
                        let ctx = AnalysisCtx {
                            image_id: t.id,
                            content_hash_hex: &t.content_hash_hex,
                            image: &native,
                            prior: &[],
                        };
                        match cap.analyze(&ctx) {
                            Ok(r) => Some((
                                t.id,
                                vec![AnalysisInput {
                                    analyzer_id: r.analyzer_id,
                                    model_version: r.model_version,
                                    payload: r.payload,
                                }],
                            )),
                            Err(e) => {
                                tracing::warn!(image_id = t.id, error = %e, "caption analysis failed");
                                None
                            }
                        }
                    });
                    if res.is_none() {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                    let n = bdone.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(32) {
                        emit_progress(app, "caption", bdone.load(Ordering::Relaxed), total_b);
                    }
                    res
                })
                .collect();
            if !results.is_empty() {
                let mut db = st.db.lock().map_err(|e| e.to_string())?;
                let now = core_library::now_epoch();
                let tx = db.conn.transaction().map_err(|e| e.to_string())?;
                for (id, inputs) in &results {
                    insert_analysis(&tx, *id, now, inputs).map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())?;
                analyzed += results.len();
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

/// Phase A→B boundary: cluster any faces added or still dirty this run (skipped when nothing needs
/// placing), then a PASSIVE WAL checkpoint. Cancellation is honored inside `cluster_assign`.
fn run_clustering(st: &AppState, faces_added: bool) -> Result<(), String> {
    if st.analysis_cancel.load(Ordering::SeqCst) {
        return Ok(());
    }
    let mut db = st.db.lock().map_err(|e| e.to_string())?;
    let dirty = has_dirty_faces(&db.conn, FACE_MODEL_TAG).map_err(|e| e.to_string())?;
    if faces_added || dirty {
        let now = core_library::now_epoch();
        cluster_assign(
            &mut db.conn,
            FACE_MODEL_TAG,
            now,
            ClusterParams::default(),
            &st.analysis_cancel,
        )
        .map_err(|e| e.to_string())?;
    }
    let _ = db.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);");
    Ok(())
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
