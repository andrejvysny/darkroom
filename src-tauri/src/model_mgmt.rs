//! Shared serde shapes for the AI-model manager UI (`models_overview` IPC). Platform-independent so
//! both the real bridges and the Intel-macOS stubs produce the same JSON contract. Data-gathering
//! lives in the per-capability bridges (`analysis` / `faces` / `segment`); this module only holds the
//! wire types + one gated helper for turning a `RemoteFile` into its on-disk status.

use serde::Serialize;

/// One downloadable model file within a capability (or a SAM tier).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFileInfo {
    /// Path relative to the models dir (e.g. `florence2/vision_encoder.onnx`).
    pub rel: String,
    /// Present on disk at >= its `min_size` floor.
    pub present: bool,
    /// On-disk byte size when present, else the `approx_size` estimate.
    pub size_bytes: u64,
}

/// One SAM quality tier (AI Masking only). Each tier is independently installable/removable.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelTierInfo {
    /// Tier tag: `"realtime"` | `"balanced"` | `"max"`.
    pub tier: String,
    /// Human label, e.g. `"Balanced · SAM ViT-B"`.
    pub label: String,
    pub installed: bool,
    /// Installed on-disk bytes, else the estimated download total.
    pub size_bytes: u64,
    pub files: Vec<ModelFileInfo>,
}

/// One user-facing AI capability (Detection & Scene / People / AI Masking).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGroup {
    /// Stable id used by cancel/remove: `"analysis"` | `"faces"` | `"mask_ai"`.
    pub id: String,
    pub name: String,
    pub description: String,
    /// False on the Intel-macOS build (AI compiled out).
    pub available: bool,
    /// Ready to use (for AI Masking: the *active* tier is installed).
    pub installed: bool,
    /// Installed on-disk bytes when installed, else the estimated download total.
    pub size_bytes: u64,
    /// Full download size (active tier for AI Masking), for the "not installed" size hint.
    pub approx_total_bytes: u64,
    /// Non-commercial / attribution note to surface on the card, if any.
    pub license: Option<String>,
    /// Flat file list (active tier for AI Masking) for the expandable detail.
    pub files: Vec<ModelFileInfo>,
    /// Per-tier detail — AI Masking only; empty for the others.
    pub tiers: Vec<ModelTierInfo>,
    /// Active SAM tier — AI Masking only.
    pub active_tier: Option<String>,
    /// Configured accelerator (CoreML / DirectML / CPU / Unavailable).
    pub accelerator: String,
}

impl ModelGroup {
    /// An "unavailable" placeholder for the Intel-macOS stub build (the only caller).
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    pub fn unavailable(id: &str, name: &str, description: &str) -> Self {
        ModelGroup {
            id: id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            available: false,
            installed: false,
            size_bytes: 0,
            approx_total_bytes: 0,
            license: None,
            files: Vec::new(),
            tiers: Vec::new(),
            active_tier: None,
            accelerator: "Unavailable".to_string(),
        }
    }
}

/// Emit a `<capability>:models` download-progress event. Keeps the legacy `done`/`total` file-count
/// fields (existing `useAnalysis`/`useFaces` banners) and adds the byte-level fields the manager reads.
/// Real-build only (`DlProgress` is a `core_analyze` type).
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub fn emit_dl<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: &str,
    p: &core_analyze::models::DlProgress,
) {
    use tauri::Emitter;
    let _ = app.emit(
        event,
        serde_json::json!({
            "done": p.file_index,
            "total": p.file_count,
            "fileName": p.file_name,
            "phase": p.phase,
            "bytesDone": p.group_bytes_done,
            "bytesTotal": p.group_bytes_total,
            "fileBytesDone": p.file_bytes_done,
            "fileBytesTotal": p.file_bytes_total,
        }),
    );
}

/// On-disk status of one `RemoteFile`. Real-build only (needs the `core_analyze` model types).
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
pub fn file_info(
    store: &core_analyze::models::ModelStore,
    f: &core_analyze::models::RemoteFile,
) -> ModelFileInfo {
    match store.path(f.rel).metadata() {
        Ok(m) if m.len() >= f.min_size => ModelFileInfo {
            rel: f.rel.to_string(),
            present: true,
            size_bytes: m.len(),
        },
        _ => ModelFileInfo {
            rel: f.rel.to_string(),
            present: false,
            size_bytes: f.approx_size,
        },
    }
}
