//! Background training + scoring for pick/reject suggestions.
//!
//! Training is minutes of CPU at worst and touches no models on disk, so it never blocks a user
//! action: the manual command and the automatic post-scan trigger both hand off to a blocking task
//! guarded by a single `suggest_running` flag, and the UI learns the result from `suggest:done` /
//! `suggest:error`.
//!
//! Scoring runs only after a *promoted* fit. A model that failed the promote gate must not rewrite
//! the badges — the whole point of the gate is that the live model keeps its own scores.

use std::sync::atomic::Ordering;

use core_app::jobs::{JobClass, JobFailure};
use core_runtime::jobs::JobContext;

use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

/// Trainable labels that must accumulate before a scan finish retrains on its own. Small enough that
/// a culling session is reflected the same day; large enough that flagging a handful of photos does
/// not re-fit (and re-score) the whole library.
const RETRAIN_LABEL_DELTA: i64 = 25;

/// Clears `suggest_running` on drop, so an error path can't wedge the gate.
struct RunGuard<R: Runtime>(AppHandle<R>);
impl<R: Runtime> Drop for RunGuard<R> {
    fn drop(&mut self) {
        self.0
            .state::<AppState>()
            .suggest_running
            .store(false, Ordering::SeqCst);
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
    let guard = RunGuard(app.clone());
    let host = app.clone();
    st.jobs
        .submit(
            crate::jobs::spec(st.inner(), JobClass::Analysis, false),
            move |context| {
                let _guard = guard;
                let result = context
                    .checkpoint()
                    .map_err(crate::jobs::cancelled)
                    .and_then(|()| run(&host, &context));
                match result {
                    Ok(outcome) => {
                        let _ = host.emit("suggest:done", &outcome);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "suggestion training failed");
                        let _ = host.emit("suggest:error", serde_json::json!({ "message": e }));
                    }
                }
                Ok(())
            },
        )
        .map_err(|e| format!("background queue: {e:?}"))?;
    Ok(())
}

/// Fit on the current labels; re-score the library when the fit was promoted.
fn run<R: Runtime>(
    app: &AppHandle<R>,
    context: &JobContext,
) -> Result<core_library::TrainOutcome, String> {
    let st = app.state::<AppState>();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let tag = crate::analysis::EMBEDDING_VERSION;

    let sequence = st.publications.sequence();
    let snapshot = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::suggest::snapshot_training(&db.conn, tag).map_err(|e| e.to_string())?
    };
    context.checkpoint().map_err(crate::jobs::cancelled)?;
    let fit =
        core_library::suggest::fit_training(snapshot, tag, now_ms).map_err(|e| e.to_string())?;
    context.checkpoint().map_err(crate::jobs::cancelled)?;
    let snapshot = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::suggest::snapshot_fitted_scoring(&db.conn, tag, &fit)
            .map_err(|e| e.to_string())?
    };
    // At most one million score pairs (~16 MB), within the Analysis reservation.
    const MAX_SCORES: usize = 1_000_000;
    let mut pages = Vec::new();
    let mut after = i64::MIN;
    let mut scored = 0usize;
    let mut seen = 0usize;
    loop {
        context.checkpoint().map_err(crate::jobs::cancelled)?;
        let page = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::suggest::scoring_page(&db.conn, tag, after).map_err(|e| e.to_string())?
        };
        seen += core_library::suggest::scoring_page_len(&page);
        if seen > MAX_SCORES {
            return Err("suggestion scoring exceeds one million rows".into());
        }
        let (last, page) = core_library::suggest::score_page(&snapshot, page);
        context.checkpoint().map_err(crate::jobs::cancelled)?;
        let Some(last) = last else { break };
        scored += core_library::suggest::scored_page_len(&page);
        pages.push(page);
        after = last;
    }
    context.checkpoint().map_err(crate::jobs::cancelled)?;
    let outcome = st
        .publications
        .with_sequence(sequence, || {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::suggest::publish_training_and_scores(&db.conn, tag, now_ms, fit, &pages)
                .map_err(|e| e.to_string())
        })
        .map_err(|_| crate::jobs::cancelled(JobFailure::Superseded))??;
    if outcome.promoted {
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
        if let Err(error) = spawn_train(app) {
            tracing::warn!(%error, "automatic suggestion training rejected");
        }
    }
}
