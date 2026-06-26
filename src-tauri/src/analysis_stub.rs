use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisStatus {
    pub total: i64,
    pub analyzed: i64,
    pub pending: i64,
    pub models_ready: bool,
    pub running: bool,
    /// AI is not built on this target (Intel macOS); mirrors the real struct's shape for the IPC.
    pub accelerator: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStats {
    pub analyzed: usize,
    pub failed: usize,
}

pub fn status(st: &AppState) -> Result<AnalysisStatus, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    let total = core_library::present_image_count(&db.conn).map_err(|e| e.to_string())?;
    Ok(AnalysisStatus {
        total,
        analyzed: 0,
        pending: total,
        models_ready: false,
        running: false,
        accelerator: "Unavailable".to_string(),
    })
}

pub fn ensure_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let _ = app.state::<AppState>();
    Err(unavailable())
}

pub fn run_pass<R: Runtime>(app: &AppHandle<R>, _force: bool) -> Result<RunStats, String> {
    let _ = app.state::<AppState>();
    Err(unavailable())
}

fn unavailable() -> String {
    "AI analysis is unavailable in the Intel macOS build".to_string()
}
