//! Background training + scoring for pick/reject suggestions.
//!
//! Training is minutes of CPU at worst and touches no models on disk, so it never blocks a user
//! action: the manual command and the automatic post-scan trigger both hand off to a blocking task
//! guarded by a single `suggest_running` flag, and the UI learns the result from `suggest:done` /
//! `suggest:error`.
//!
//! Scoring runs only after a *promoted* fit. A model that failed the promote gate must not rewrite
//! the badges — the whole point of the gate is that the live model keeps its own scores.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Trainable labels that must accumulate before a scan finish retrains on its own. Small enough that
/// a culling session is reflected the same day; large enough that flagging a handful of photos does
/// not re-fit (and re-score) the whole library.
const RETRAIN_LABEL_DELTA: i64 = 25;

/// Clears `suggest_running` on drop, so an error path can't wedge the gate.
struct RunGuard<'a>(&'a AtomicBool);
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub fn is_running(st: &AppState) -> bool {
    st.suggest_running.load(Ordering::SeqCst)
}

/// Train (and, if promoted, re-score) in the background. Returns immediately; the result arrives as
/// `suggest:done` (a `TrainOutcome`) or `suggest:error` (`{message}`).
pub fn spawn_train<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let st = app.state::<AppState>();
    // Checked here, not inside the task, so a manual "Train" can report "already running" instead of
    // resolving as if it had started one.
    if st.suggest_running.swap(true, Ordering::SeqCst) {
        return Err("suggestion training is already running".into());
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let _guard = RunGuard(&st.suggest_running);
        match run(&app) {
            Ok(outcome) => {
                let _ = app.emit("suggest:done", &outcome);
            }
            Err(e) => {
                tracing::warn!(error = %e, "suggestion training failed");
                let _ = app.emit("suggest:error", serde_json::json!({ "message": e }));
            }
        }
    });
    Ok(())
}

/// Fit on the current labels; re-score the library when the fit was promoted.
fn run<R: Runtime>(app: &AppHandle<R>) -> Result<core_library::TrainOutcome, String> {
    let st = app.state::<AppState>();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let tag = crate::analysis::EMBEDDING_VERSION;

    let outcome = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::train_and_store(&db.conn, tag, now_ms).map_err(|e| e.to_string())?
    };
    if outcome.promoted {
        // Deliberately a separate lock acquisition: the scoring pass commits in batches so the grid
        // stays responsive, and holding the catalog across the whole fit + score would freeze it.
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let scored = core_library::score_all(&db.conn, tag, now_ms).map_err(|e| e.to_string())?;
        tracing::info!(
            model = outcome.model_id,
            scored,
            "suggestion model promoted"
        );
    }
    Ok(outcome)
}

/// Post-scan trigger: retrain only when there is enough NEW evidence to justify it.
///
/// Fresh embeddings alone are not a reason to re-fit (the labels are unchanged), but a first
/// trainable library is — otherwise a user who culled before ever scanning would need to find the
/// manual action to get any suggestions at all.
pub fn maybe_spawn_train<R: Runtime>(app: &AppHandle<R>) {
    let st = app.state::<AppState>();
    let status = {
        let Ok(db) = st.db.lock() else { return };
        match core_library::suggest_status(&db.conn, crate::analysis::EMBEDDING_VERSION) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "suggestion status unavailable");
                return;
            }
        }
    };
    if !status.trainable {
        return;
    }
    // A large DROP in labels (a mass un-flag) invalidates the model as surely as a gain.
    let stale = status.model_id.is_none() || status.labels_delta.abs() >= RETRAIN_LABEL_DELTA;
    if stale {
        let _ = spawn_train(app);
    }
}
