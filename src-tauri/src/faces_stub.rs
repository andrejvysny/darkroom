use serde::Serialize;
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

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
    pub cluster: core_library::ClusterStats,
}

pub fn status(st: &AppState) -> Result<FacesStatus, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    let total = core_library::present_image_count(&db.conn).map_err(|e| e.to_string())?;
    let (faces, people) = core_library::faces_summary(&db.conn).map_err(|e| e.to_string())?;
    Ok(FacesStatus {
        total,
        processed: 0,
        pending: total,
        models_ready: false,
        running: false,
        faces,
        people,
    })
}

pub fn ensure_face_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let _ = app.state::<AppState>();
    Err(unavailable())
}

pub fn run_pass<R: Runtime>(app: &AppHandle<R>, _force: bool) -> Result<FacesRunStats, String> {
    let _ = app.state::<AppState>();
    Err(unavailable())
}

fn unavailable() -> String {
    "Face analysis is unavailable in the Intel macOS build".to_string()
}
