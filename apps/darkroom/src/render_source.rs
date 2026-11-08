//! Steps shared by the background renderers (edited thumbnails, rendered previews): interpret a
//! stored edit, decode the verified source at full resolution, and render it through the host's
//! pipeline. Kept in one place so the two renderers cannot drift in what they refuse or how they
//! size the source.

use std::path::Path;
use std::sync::Arc;

use crate::photo_worker::unsupported_reason;
use core_model::document::{EditDocument, ReadStatus};
use core_model::DevelopParams;
use core_pipeline::{DevelopPipeline, GpuContext};
use core_raw::LinearImage;

/// The parameters a stored edit describes, or why this build must not render it.
pub fn interpret_edit(json: String, stored_version: i64) -> Result<DevelopParams, String> {
    let document = EditDocument::read(json, stored_version);
    let params = match (document.status(), document.preview()) {
        (ReadStatus::Supported, Some(params)) => params.clone(),
        _ => return Err("the stored edit cannot be interpreted by this build".into()),
    };
    match unsupported_reason(&params) {
        Some(reason) => Err(format!("{reason} edits are not rendered by this host yet")),
        None => Ok(params),
    }
}

/// Read `path`, require its bytes to hash to `expected_hash`, and decode them at FULL resolution.
/// Never the proxy decode: pixels that reach a disk tier must be the real ones.
pub fn decode_checked(path: &Path, expected_hash: &str) -> Result<LinearImage, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if core_raw::hex(&core_raw::content_hash(&bytes)) != expected_hash {
        return Err(format!(
            "{} no longer matches the catalogued content",
            path.display()
        ));
    }
    let source = core_raw::source_from_bytes(Arc::new(bytes), path);
    core_raw::develop_linear(&source)
        .map_err(|error| format!("could not decode {}: {error}", path.display()))
}

/// Render `linear` with `params` (crop-aware, no AI coverages) and fit the longest edge to `edge`;
/// never upscales. Returns RGBA8 pixels and their size.
pub fn render_rgba(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    linear: LinearImage,
    params: &mut DevelopParams,
    path: &Path,
    edge: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    params.display_referred = core_raw::is_display(path);
    let source_edge =
        params
            .crop
            .preview_source_edge(linear.width, linear.height, edge, ctx.max_texture_dim);
    let linear = linear.downscale_into_hq(source_edge);
    let (rgba, width, height) = pipeline
        .render_cropped_with_ai_coverages(ctx, &linear, params, &[])
        .map_err(|error| error.to_string())?;
    Ok(if width.max(height) > edge {
        core_pipeline::resize_rgba8(&rgba, width, height, edge)
    } else {
        (rgba, width, height)
    })
}
