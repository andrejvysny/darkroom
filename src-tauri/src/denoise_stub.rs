//! Stub for the AI-denoise backend on the Intel-macOS build (no ort/ONNX stack, no `core-analyze`).
//! Mirrors the real `denoise.rs` surface so `commands.rs` compiles unchanged; every op is unavailable.

use core_raw::{LinearImage, RawSource};
use tauri::{AppHandle, Runtime};

use crate::state::AppState;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseStatus {
    pub running: bool,
    pub available: bool,
    pub accelerator: &'static str,
}

pub fn status(_st: &AppState) -> DenoiseStatus {
    DenoiseStatus {
        running: false,
        available: false,
        accelerator: "CPU",
    }
}

pub fn apply<R: Runtime>(_app: &AppHandle<R>, _image_id: i64, _amount: f32) -> Result<(), String> {
    Err(unavailable())
}

pub fn clear(_st: &AppState, _image_id: i64) -> Result<(), String> {
    Ok(())
}

pub fn denoised_full(
    _st: &AppState,
    _src: &RawSource,
    _amount: f32,
) -> Result<LinearImage, String> {
    Err(unavailable())
}

fn unavailable() -> String {
    "AI denoise is unavailable in the Intel macOS build".to_string()
}
