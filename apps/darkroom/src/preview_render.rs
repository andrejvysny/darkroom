//! The host's half of the preview service: renders one photo through the GPU develop pipeline into
//! a large preview JPEG plus a 1024 px thumbnail JPEG, and leaves scheduling, publication and the
//! cache to `core_runtime::preview::PreviewService`.
//!
//! The saved edit is re-read here (not trusted from the job): the render reports which edit
//! version it used and the service refuses to publish when the catalog has moved on since.

use std::sync::{Arc, PoisonError};

use crate::render_source::{decode_checked, interpret_edit, render_rgba};
use core_model::DevelopParams;
use core_pipeline::{DevelopPipeline, GpuContext};
use core_runtime::catalog::CatalogAdapter;
use core_runtime::preview::{PreviewJob, PreviewRenderError, PreviewRenderPort, RenderedPreview};

/// Large-preview JPEG quality: viewed full-screen, so a touch above the thumbnail's.
const PREVIEW_QUALITY: u8 = 88;
const THUMB_EDGE: u32 = 1024;
const THUMB_QUALITY: u8 = 85;

/// Whether the interactive photo worker has work; renders yield to it.
pub type BusyProbe = Arc<dyn Fn() -> bool + Send + Sync>;

pub struct PreviewRenderer {
    pub catalog: Arc<CatalogAdapter>,
    pub ctx: GpuContext,
    pub pipeline: Arc<DevelopPipeline>,
    pub busy: BusyProbe,
}

impl PreviewRenderPort for PreviewRenderer {
    fn render(&self, job: &PreviewJob) -> Result<RenderedPreview, PreviewRenderError> {
        use PreviewRenderError::{Failed, Skip};
        // The GPU is shared with the interactive path: a background render must never delay it.
        if (self.busy)() {
            return Err(Skip("the photo worker is busy".into()));
        }
        let (mut params, version) = self.saved_edit(job.image_id)?;
        let linear = decode_checked(&job.path, &job.hash).map_err(Failed)?;
        let (rgba, width, height) = render_rgba(
            &self.ctx,
            &self.pipeline,
            linear,
            &mut params,
            &job.path,
            job.edge,
        )
        .map_err(Failed)?;
        let (preview_jpeg, thumb_jpeg) = encode_tiers(&rgba, width, height).map_err(Failed)?;
        Ok(RenderedPreview {
            preview_jpeg,
            thumb_jpeg,
            rendered_edit_version: version,
        })
    }
}

impl PreviewRenderer {
    /// The edit as saved right now: its parameters and version, or the defaults for an unedited
    /// photo. An edit this host cannot interpret is `Unsupported`, never rendered as defaults.
    fn saved_edit(
        &self,
        image_id: i64,
    ) -> Result<(DevelopParams, Option<i64>), PreviewRenderError> {
        use PreviewRenderError::{Failed, Unsupported};
        let (_, database) = self
            .catalog
            .shared()
            .map_err(|error| Failed(format!("no catalog: {error}")))?;
        let stored = {
            let db = database.lock().unwrap_or_else(PoisonError::into_inner);
            core_library::edits::get_edit_document_row(&db.conn, image_id)
                .map_err(|error| Failed(format!("could not read the edit: {error}")))?
        };
        match stored {
            Some((json, process_version, updated_at)) => interpret_edit(json, process_version)
                .map(|params| (params, Some(updated_at)))
                .map_err(Unsupported),
            None => Ok((DevelopParams::default(), None)),
        }
    }
}

/// The preview JPEG from the full-size pixels, and the 1024 px thumbnail from the same pixels
/// (one GPU render feeds both tiers).
fn encode_tiers(rgba: &[u8], width: u32, height: u32) -> Result<(Vec<u8>, Vec<u8>), String> {
    let preview = core_pipeline::rgba8_to_jpeg(rgba, width, height, PREVIEW_QUALITY)
        .map_err(|error| error.to_string())?;
    let thumb = if width.max(height) > THUMB_EDGE {
        let (small, w, h) = core_pipeline::resize_rgba8(rgba, width, height, THUMB_EDGE);
        core_pipeline::rgba8_to_jpeg(&small, w, h, THUMB_QUALITY)
    } else {
        core_pipeline::rgba8_to_jpeg(rgba, width, height, THUMB_QUALITY)
    }
    .map_err(|error| error.to_string())?;
    Ok((preview, thumb))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_is_capped_at_1024_and_preview_keeps_its_size() {
        let (w, h) = (2000_u32, 1000_u32);
        let rgba = vec![128_u8; (w * h * 4) as usize];
        let (preview, thumb) = encode_tiers(&rgba, w, h).unwrap();
        let size = |bytes: &[u8]| {
            let image = image::load_from_memory(bytes).unwrap();
            (image.width(), image.height())
        };
        assert_eq!(size(&preview), (2000, 1000));
        assert_eq!(size(&thumb), (1024, 512));
    }
}
