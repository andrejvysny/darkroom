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

/// No AI stage can run on this target, but panorama detection is pure geometry over cached
/// thumbnails — so a panorama-only scan stays available here.
pub fn stage_models_ready(_st: &AppState, stage: core_library::StageId) -> bool {
    matches!(stage, core_library::StageId::Panoramas)
}

pub fn run_pass<R: Runtime>(
    app: &AppHandle<R>,
    _force: bool,
    _scope: &core_library::ScanScope,
    _stages: &[core_library::StageId],
) -> Result<RunStats, String> {
    let _ = app.state::<AppState>();
    Err(unavailable())
}

/// Scope sizing still works without AI: the image count is a plain catalog query. Every AI stage
/// reports "models not ready" so the modal disables those rows; panorama stays usable.
pub fn scope_counts(
    st: &AppState,
    scope: &core_library::ScanScope,
) -> Result<core_library::ScopeCounts, String> {
    use core_library::StageId;
    let db = st.db.lock().map_err(|e| e.to_string())?;
    let total = core_library::scope_image_ids(&db.conn, scope)
        .map_err(|e| e.to_string())?
        .len() as i64;
    let stages = StageId::ALL
        .iter()
        .map(|&stage| {
            let ready = stage_models_ready(st, stage);
            core_library::StagePending {
                stage,
                pending: if stage == StageId::Panoramas {
                    core_library::pano_detect::pending_count(
                        &db.conn,
                        core_library::pano_detect::ALGO_VERSION,
                    )
                    .ok()
                } else {
                    None
                },
                models_ready: ready,
                library_wide: stage == StageId::Panoramas,
            }
        })
        .collect();
    Ok(core_library::ScopeCounts { total, stages })
}

/// Panorama state is still recorded on this target, so the per-photo readout works.
pub fn image_scan_state(
    st: &AppState,
    image_id: i64,
) -> Result<core_library::ImageScanState, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    core_library::image_scan_state(
        &db.conn,
        image_id,
        &[(
            core_library::PANORAMA_STAGE_ID,
            core_library::pano_detect::ALGO_VERSION,
        )],
    )
    .map_err(|e| e.to_string())
}

pub fn overview(_st: &AppState) -> crate::model_mgmt::ModelGroup {
    crate::model_mgmt::ModelGroup::unavailable(
        "analysis",
        "Detection & Scene",
        "Detects objects, animals & scenes and writes captions/keywords for search.",
    )
}

pub fn remove(_st: &AppState) -> Result<(), String> {
    Err(unavailable())
}

fn unavailable() -> String {
    "AI analysis is unavailable in the Intel macOS build".to_string()
}
