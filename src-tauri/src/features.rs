//! One-shot backfill of per-image `image_features` (model inputs for lighting/best-shot/dedup).
//! Explicit/lazy (not run on every import). Decode + compute run UNLOCKED in parallel; rows are
//! written in brief batched transactions so `library_query` stays responsive.

use std::path::Path;

use rayon::prelude::*;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

const BATCH: usize = 16;

/// Compute + persist features for every present image that lacks a row. Returns the count computed.
/// Emits `features:progress` `{done,total}` and a final `features:done` `{computed}`.
pub fn run_backfill<R: Runtime>(app: &AppHandle<R>) -> Result<usize, String> {
    let st = app.state::<AppState>();

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
            for (id, f) in &batch {
                core_library::set_image_features(&db.conn, *id, f, now)
                    .map_err(|e| e.to_string())?;
            }
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
