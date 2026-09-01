//! Backfill of per-image `image_features` (model inputs for lighting/best-shot/dedup).
//!
//! Runs automatically after an import commit and after each AI scan (see [`spawn_backfill`]), plus
//! on demand from Settings — the pass is a no-op for images that already have a row, so the
//! automatic triggers cost one query when there is nothing to do. Decode + compute run UNLOCKED in
//! parallel; rows are written in brief batched transactions so `library_query` stays responsive.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

const BATCH: usize = 16;

/// Clears `features_running` on drop, so an error path can't wedge the gate.
struct RunGuard<'a>(&'a AtomicBool);
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Kick off a backfill in the background and forget it — the automatic trigger.
///
/// Deliberately fire-and-forget: neither an import nor a scan should block on (or fail because of)
/// feature computation, and a second trigger while one is in flight is a no-op rather than a queued
/// duplicate pass. The images the first pass missed are simply picked up by the next trigger.
pub fn spawn_backfill<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || match run_backfill(&app) {
        Ok(n) if n > 0 => tracing::info!(computed = n, "feature backfill finished"),
        Err(e) => tracing::debug!(error = %e, "feature backfill skipped"),
        _ => {}
    });
}

/// Compute + persist features for every present image that lacks a row. Returns the count computed.
/// Emits `features:progress` `{done,total}` and a final `features:done` `{computed}`.
pub fn run_backfill<R: Runtime>(app: &AppHandle<R>) -> Result<usize, String> {
    let st = app.state::<AppState>();
    // Two passes over the same `images_missing_features` set would decode every image twice and
    // race each other to the same rows.
    if st.features_running.swap(true, Ordering::SeqCst) {
        return Err("feature backfill already running".into());
    }
    let _guard = RunGuard(&st.features_running);

    let todo: Vec<(i64, String)> = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::images_missing_features(&db.conn).map_err(|e| e.to_string())?
    };
    let total = todo.len();
    let _ = app.emit(
        "features:progress",
        serde_json::json!({"done": 0, "total": total}),
    );

    let mut computed = 0usize;
    for chunk in todo.chunks(BATCH) {
        // Unlocked parallel decode + compute (2 raw decodes per image: linear preview + as-shot WB).
        let batch: Vec<(i64, core_library::ImageFeatures)> = chunk
            .par_iter()
            .filter_map(|(id, path)| {
                let src = core_raw::source_from_path(Path::new(path)).ok()?;
                let lin = core_raw::develop_linear_preview(&src).ok()?;
                let wb = core_raw::as_shot_wb(&src).unwrap_or([1.0; 4]);
                Some((*id, core_library::compute_features(&lin, wb)))
            })
            .collect();

        let now = core_library::now_epoch();
        {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            // One transaction per chunk: atomic (no half-written batch on error) and a single fsync
            // instead of one per row, matching the index/analysis write passes.
            let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
            for (id, f) in &batch {
                core_library::set_image_features(&tx, *id, f, now).map_err(|e| e.to_string())?;
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        computed += batch.len();
        let _ = app.emit(
            "features:progress",
            serde_json::json!({"done": computed, "total": total}),
        );
    }

    let _ = app.emit("features:done", serde_json::json!({"computed": computed}));
    Ok(computed)
}
