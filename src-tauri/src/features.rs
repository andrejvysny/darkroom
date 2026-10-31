//! Backfill of per-image `image_features` (model inputs for lighting/best-shot/dedup).
//!
//! Runs automatically after an import commit and after each AI scan (see [`spawn_backfill`]), plus
//! on demand from Settings — the pass is a no-op for images that already have a row, so the
//! automatic triggers cost one query when there is nothing to do. Decode + compute run UNLOCKED in
//! sequentially; rows are written in brief batched transactions so `library_query` stays responsive.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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

/// A single retained trigger survives admission backpressure without blocking an import worker.
#[derive(Default)]
pub struct BackfillQueue(Mutex<(bool, bool)>);
impl BackfillQueue {
    fn request(&self) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = true;
        if state.1 {
            return false;
        }
        state.1 = true;
        true
    }
    fn take(&self) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.0 {
            state.1 = false;
            return false;
        }
        state.0 = false;
        true
    }
    fn stop(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = (false, false);
    }
}

pub fn spawn_backfill<R: Runtime>(app: &AppHandle<R>) {
    let st = app.state::<AppState>();
    if st.workers.is_stopping() || !st.features_queue.request() {
        return;
    }
    let host = app.clone();
    let result = crate::jobs::worker(app, "feature-backfill-admission", move || {
        let st = host.state::<AppState>();
        while st.features_queue.take() {
            match admit_backfill(&host) {
                Ok(true) => {}
                Ok(false) => {
                    st.features_queue.stop();
                    break;
                }
                Err(error) => {
                    st.features_queue.stop();
                    return Err(error);
                }
            }
        }
        Ok(())
    });
    if let Err(error) = result {
        st.features_queue.stop();
        tracing::warn!(%error, "feature backfill admission failed");
    }
}

fn admit_backfill<R: Runtime>(app: &AppHandle<R>) -> Result<bool, String> {
    use core_app::jobs::{JobClass, SubmitError};
    let st = app.state::<AppState>();
    loop {
        let spec = crate::jobs::spec(st.inner(), JobClass::Analysis, false);
        if st.workers.is_stopping() || !st.jobs.wait_capacity(spec.estimated_bytes) {
            return Ok(false);
        }
        let host = app.clone();
        match st.jobs.submit(spec, move |context| {
            context.checkpoint()?;
            run_backfill(&host).map_err(core_app::jobs::JobFailure::Failed)
        }) {
            Ok(job) => {
                match job.wait() {
                    Ok(n) if n > 0 => tracing::info!(computed = n, "feature backfill finished"),
                    Err(error) => tracing::debug!(?error, "feature backfill skipped"),
                    _ => {}
                }
                return Ok(true);
            }
            Err(SubmitError::Capacity) => continue,
            Err(SubmitError::Stopped) => return Ok(false),
            Err(error) => return Err(format!("feature backfill admission: {error:?}")),
        }
    }
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

    let epoch = st.publications.epoch();
    let total = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::count_images_missing_features(&db.conn).map_err(|e| e.to_string())?
    };
    let _ = app.emit(
        "features:progress",
        serde_json::json!({"done": 0, "total": total}),
    );

    let mut computed = 0usize;
    let mut cursor = i64::MIN;
    loop {
        if st.workers.is_stopping() {
            return Err("feature backfill stopped: shutdown".into());
        }
        if st.publications.epoch() != epoch {
            return Err("feature backfill stale: catalog reset".into());
        }
        let sequence = st.publications.sequence();
        let (chunk, keys) = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let chunk = core_library::images_missing_features_after(&db.conn, cursor, BATCH as i64)
                .map_err(|e| e.to_string())?;
            let keys = chunk
                .iter()
                .map(|(id, path)| {
                    let row = core_library::image_by_id(&db.conn, *id)
                        .map_err(|e| e.to_string())?
                        .ok_or_else(|| "feature backfill stale: source missing".to_string())?;
                    if row.path != *path {
                        return Err("feature backfill stale: source changed".into());
                    }
                    Ok((
                        *id,
                        core_runtime::publication::SourceKey {
                            path: row.path,
                            hash: row.content_hash,
                        },
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            (chunk, keys)
        };
        if st.publications.epoch() != epoch {
            return Err("feature backfill stale: catalog reset".into());
        }
        if chunk.is_empty() {
            break;
        }
        cursor = chunk.last().expect("nonempty chunk").0;
        let tokens = keys
            .into_iter()
            .map(|(id, key)| {
                st.publications
                    .capture_at(core_model::identity::ImageId(id), key, sequence)
                    .map_err(|_| "feature backfill stale: source changed".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        // Unlocked sequential decode + compute (2 raw decodes per image: linear preview + as-shot WB).
        let batch: Vec<(i64, core_library::ImageFeatures)> = chunk
            .iter()
            .filter_map(|(id, path)| {
                let src = core_raw::source_from_path(Path::new(path)).ok()?;
                let lin = core_raw::develop_linear_preview(&src).ok()?;
                let wb = core_raw::as_shot_wb(&src).unwrap_or([1.0; 4]);
                Some((*id, core_library::compute_features(&lin, wb)))
            })
            .collect();

        let now = core_library::now_epoch();
        st.publications
            .with_tokens(&tokens, || -> Result<(), String> {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                // One transaction per chunk: atomic (no half-written batch on error) and a single fsync
                // instead of one per row, matching the index/analysis write passes.
                let tx = db.conn.unchecked_transaction().map_err(|e| e.to_string())?;
                for (id, f) in &batch {
                    core_library::set_image_features(&tx, *id, f, now)
                        .map_err(|e| e.to_string())?;
                }
                tx.commit().map_err(|e| e.to_string())?;
                Ok(())
            })
            .map_err(|_| "feature backfill stale: source changed".to_string())??;
        computed += batch.len();
        let _ = app.emit(
            "features:progress",
            serde_json::json!({"done": computed, "total": total}),
        );
    }

    let _ = app.emit("features:done", serde_json::json!({"computed": computed}));
    Ok(computed)
}

#[cfg(test)]
mod tests {
    use super::BackfillQueue;

    #[test]
    fn bursts_retain_one_trigger_until_the_admission_worker_finishes() {
        let queue = BackfillQueue::default();
        assert!(queue.request());
        assert!(queue.take());
        for _ in 0..10000 {
            assert!(!queue.request());
        }
        assert!(queue.take());
        assert!(!queue.take());
        assert!(queue.request());
        queue.stop();
        assert!(!queue.take());
    }
}
