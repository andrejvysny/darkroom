//! On-demand raw-domain AI denoise orchestration.
//!
//! Denoise runs the (seconds-long) raw-mosaic denoise ONCE per image, then swaps the resulting
//! denoised `PreparedImage` straight into `full_render_cache` (keyed by `image_id`, same shape the
//! clean decode uses). This is the whole trick: `develop_render` / `ensure_*_render_cache` stay
//! byte-for-byte unchanged — they simply find a denoised texture for the id and serve it at every
//! zoom (the full-res source path wins once the full cache is warm). Changing the blend `amount`
//! re-lerps the cached clean+denoised buffers (no re-inference) and re-uploads.
//!
//! The neural ONNX model is a drop-in for `CpuDenoiser` behind `core_raw::MosaicDenoiser`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use core_analyze::denoise::CpuDenoiser;
use core_raw::LinearImage;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::commands::FULL_MAX_EDGE;
use crate::state::AppState;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DenoiseStatus {
    pub running: bool,
    /// `true` on builds that ship the inference stack (false on the Intel-macOS stub).
    pub available: bool,
    /// The denoiser in use — `"CPU"` for the classical placeholder (a neural model would report its EP).
    pub accelerator: &'static str,
}

pub fn status(st: &AppState) -> DenoiseStatus {
    DenoiseStatus {
        running: st.denoise_running.load(Ordering::SeqCst),
        available: true,
        accelerator: if core_analyze::denoise::pmrid_available() {
            core_analyze::accelerator()
        } else {
            "CPU"
        },
    }
}

/// The neural PMRID denoiser if this build embedded the model (ort Session built on first use and
/// cached on `AppState`), else `None` → callers fall back to `CpuDenoiser`.
fn pmrid_denoiser(st: &AppState) -> Option<Arc<core_analyze::denoise::PmridDenoiser>> {
    let mut slot = st.denoiser.lock().ok()?;
    if slot.is_none() {
        *slot = core_analyze::denoise::try_pmrid().map(Arc::new);
    }
    slot.clone()
}

/// Run the raw-domain denoise for `src` with the best available denoiser — neural PMRID if embedded,
/// else the classical `CpuDenoiser`. Returns the clean + denoised full-res linear develop.
fn denoise_output(
    st: &AppState,
    src: &core_raw::RawSource,
) -> Result<core_raw::DenoiseOutput, String> {
    match pmrid_denoiser(st) {
        Some(d) => core_raw::develop_linear_denoised(src, d.as_ref()),
        None => core_raw::develop_linear_denoised(src, &CpuDenoiser { strength: 1.0 }),
    }
    .map_err(|e| e.to_string())
}

/// Resets `denoise_running` + `denoise_cancel` on drop, so an early return / error / cancel can't
/// wedge the guard or leave a stale cancel request.
struct RunGuard<'a> {
    running: &'a AtomicBool,
    cancel: &'a AtomicBool,
}
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.cancel.store(false, Ordering::SeqCst);
    }
}

fn emit_phase<R: Runtime>(app: &AppHandle<R>, phase: &str) {
    let _ = app.emit("denoise:progress", serde_json::json!({ "phase": phase }));
}

/// Per-pixel linear blend `clean + (denoised - clean) * a`, `a` in 0..1. Both buffers share dims
/// (same source, same develop), so the lengths match.
fn blend(clean: &LinearImage, denoised: &LinearImage, a: f32) -> LinearImage {
    if a >= 1.0 {
        return denoised.clone();
    }
    if a <= 0.0 {
        return clean.clone();
    }
    let data = clean
        .data
        .iter()
        .zip(denoised.data.iter())
        .map(|(c, d)| c + (d - c) * a)
        .collect();
    LinearImage {
        width: clean.width,
        height: clean.height,
        data,
    }
}

/// Apply (or re-blend) denoise for `image_id` at `amount` (0..100). If clean+denoised buffers are
/// already cached for this image (a prior apply), only re-blends + re-uploads (fast, no inference).
/// Otherwise decodes + runs the denoiser. On success the denoised prepared image is swapped into
/// `full_render_cache`; the caller then triggers a `develop_render` to display it. Emits
/// `denoise:progress` (phase markers) and `denoise:done`.
pub fn apply<R: Runtime>(app: &AppHandle<R>, image_id: i64, amount: f32) -> Result<(), String> {
    let st = app.state::<AppState>();
    let gpu = st
        .gpu
        .as_ref()
        .ok_or_else(|| "GPU develop unavailable".to_string())?;
    if st.denoise_running.swap(true, Ordering::SeqCst) {
        return Err("denoise already running".into());
    }
    st.denoise_cancel.store(false, Ordering::SeqCst);
    let _guard = RunGuard {
        running: &st.denoise_running,
        cancel: &st.denoise_cancel,
    };
    let a = (amount / 100.0).clamp(0.0, 1.0);

    // Reuse cached buffers if this image was already denoised (amount change → no re-inference).
    let cached = {
        let slot = st.denoise_cache.lock().map_err(|e| e.to_string())?;
        slot.as_ref()
            .filter(|(id, _, _)| *id == image_id)
            .map(|(_, c, d)| (c.clone(), d.clone()))
    };
    let (clean, denoised) = match cached {
        Some(pair) => pair,
        None => {
            emit_phase(app, "decode");
            let path = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::image_by_id(&db.conn, image_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "image not found".to_string())?
                    .path
            };
            let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
            if st.denoise_cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            emit_phase(app, "denoise");
            let t = Instant::now();
            let out = denoise_output(&st, &src)?;
            let denoised = out.denoised.ok_or_else(|| {
                "this sensor is not supported for denoise (non-Bayer)".to_string()
            })?;
            tracing::info!(
                elapsed_ms = t.elapsed().as_millis(),
                image_id,
                "denoise compute"
            );
            // Downscale both to the render cap so the prepared texture matches the clean path's size.
            let clean = Arc::new(out.clean.downscale_into_hq(FULL_MAX_EDGE));
            let denoised = Arc::new(denoised.downscale_into_hq(FULL_MAX_EDGE));
            *st.denoise_cache.lock().map_err(|e| e.to_string())? =
                Some((image_id, clean.clone(), denoised.clone()));
            (clean, denoised)
        }
    };
    if st.denoise_cancel.load(Ordering::SeqCst) {
        return Err("cancelled".into());
    }

    // Blend → prepare → swap into the full render cache, but only while it's still the current image
    // (the user may have navigated away during the compute; the buffers stay cached for their return).
    emit_phase(app, "upload");
    let blended = blend(clean.as_ref(), denoised.as_ref(), a);
    if st.current_image.load(Ordering::SeqCst) == image_id {
        let prepared = gpu
            .pipeline
            .prepare(&gpu.ctx, &blended)
            .map_err(|e| e.to_string())?;
        *st.full_render_cache.lock().map_err(|e| e.to_string())? =
            Some((image_id, Arc::new(prepared)));
        // Drop any clean preview for this id so the full (denoised) path is what gets served.
        let mut slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _)| *id == image_id) {
            *slot = None;
        }
    }
    let _ = app.emit("denoise:done", serde_json::json!({ "imageId": image_id }));
    Ok(())
}

/// Full-resolution denoised + blended linear develop for export (NO downscale — export needs the
/// source-sized buffer). `amount` 0..100. Non-Bayer sensors return the clean develop (denoise silently
/// inapplicable), matching the develop-view fallback.
pub fn denoised_full(
    st: &AppState,
    src: &core_raw::RawSource,
    amount: f32,
) -> Result<LinearImage, String> {
    let a = (amount / 100.0).clamp(0.0, 1.0);
    let out = denoise_output(st, src)?;
    Ok(match out.denoised {
        Some(d) => blend(&out.clean, &d, a),
        None => out.clean,
    })
}

/// Turn denoise off for `image_id`: drop its cached buffers and evict the (denoised) render caches so
/// the next `develop_render` re-decodes the clean image.
pub fn clear(st: &AppState, image_id: i64) -> Result<(), String> {
    {
        let mut slot = st.denoise_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _, _)| *id == image_id) {
            *slot = None;
        }
    }
    {
        let mut slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _)| *id == image_id) {
            *slot = None;
        }
    }
    {
        let mut slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _)| *id == image_id) {
            *slot = None;
        }
    }
    Ok(())
}
