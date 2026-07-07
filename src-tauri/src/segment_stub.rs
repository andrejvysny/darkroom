//! Stub for the AI-masking segmentation backend on the Intel-macOS build (no ort/ONNX stack). Mirrors
//! the real `segment.rs` surface so `commands.rs` compiles unchanged; every op is unavailable.

use core_pipeline::{AiCoverage, AiPoint, DevelopParams};
use tauri::{AppHandle, Runtime};

use crate::state::AppState;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    pub hash: String,
    pub width: u32,
    pub height: u32,
    pub iou: f32,
}

pub fn models_ready(_st: &AppState) -> bool {
    false
}

pub fn ensure_models<R: Runtime>(_app: &AppHandle<R>) -> Result<(), String> {
    Err(unavailable())
}

pub fn prompt(
    _st: &AppState,
    _image_id: i64,
    _points: Vec<AiPoint>,
) -> Result<PromptResult, String> {
    Err(unavailable())
}

pub fn warm(_st: &AppState, _image_id: i64) -> Result<(), String> {
    Err(unavailable())
}

pub fn coverages_for(_st: &AppState, _image_id: i64, _params: &DevelopParams) -> Vec<AiCoverage> {
    Vec::new()
}

pub fn overview(_st: &AppState) -> crate::model_mgmt::ModelGroup {
    crate::model_mgmt::ModelGroup::unavailable(
        "mask_ai",
        "AI Masking",
        "Click a subject in Develop to auto-select it as a mask (SAM).",
    )
}

pub fn remove(_st: &AppState, _tier: &str) -> Result<(), String> {
    Err(unavailable())
}

fn unavailable() -> String {
    "AI masking is unavailable in the Intel macOS build".to_string()
}
