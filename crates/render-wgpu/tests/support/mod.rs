//! Shared GPU-gated helpers for render-wgpu tests. Synthetic images only — no RAW fixtures.
#![allow(dead_code)] // Each test binary uses a subset of these helpers.

use core_model::identity::{
    CacheVersion, CatalogEpoch, DeviceEpoch, DisplayRevision, EditRevision, EditorSession,
    FrameKey, ImageId, ProcessVersion, QualityTier, RenderIdentity, RendererVersion,
    SelectionGeneration, SourceIdentity, SourceRevision, ViewportRevision,
};
use core_pipeline::{DevelopParams, GpuContext, PipelineError, ViewParams};
use core_raw::LinearImage;

/// Headless local runs may skip when no adapter exists; `DARKROOM_REQUIRE_FIXTURES=1` (the CI
/// pixel-job gate) or `DARKROOM_REQUIRE_GPU=1` make a missing adapter a hard failure.
pub fn gpu() -> Option<GpuContext> {
    match GpuContext::new() {
        Ok(ctx) => Some(ctx),
        Err(PipelineError::NoAdapter)
            if std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none()
                && std::env::var_os("DARKROOM_REQUIRE_GPU").is_none() =>
        {
            eprintln!("no GPU adapter — skipping (set DARKROOM_REQUIRE_FIXTURES=1 to require one)");
            None
        }
        Err(error) => panic!(
            "GPU initialization failed: {error}; DARKROOM_REQUIRE_FIXTURES=1 requires an adapter"
        ),
    }
}

/// Smooth gradient with all channels varying, so develop output is non-flat and any mix-up
/// between concurrent renders changes bytes.
pub fn ramp(w: u32, h: u32) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(x as f32 / (w - 1).max(1) as f32);
            data.push(y as f32 / (h - 1).max(1) as f32);
            data.push((x + y) as f32 / (w + h - 2).max(1) as f32);
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

/// Active viewport view: whole crop window into an `out_w × out_h` target, overlay off — the
/// presentation view `Renderer::render` develops with.
pub fn view(out_w: u32, out_h: u32) -> ViewParams {
    ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w,
        out_h,
        active: true,
        overlay_layer: -1,
        overlay_color: [0.85, 0.10, 0.10],
        overlay_strength: 0.5,
        crop_preview: false,
    }
}

/// A few non-default params (exposure + contrast + a real crop/straighten).
pub fn params_a() -> DevelopParams {
    DevelopParams {
        exposure: 0.6,
        contrast: 30.0,
        crop: core_pipeline::Crop {
            cx: 0.55,
            cy: 0.45,
            hw: 0.4,
            hh: 0.35,
            angle: 0.04,
            rot90: 0,
        },
        ..Default::default()
    }
}

/// Visibly different from [`params_a`]: inverse tone, saturation.
pub fn params_b() -> DevelopParams {
    DevelopParams {
        exposure: -0.8,
        contrast: -25.0,
        saturation: 60.0,
        ..Default::default()
    }
}

/// Builder for `FrameKey`s in presenter/pool tests: everything defaults to the "image 0,
/// session 0, rev 0" frame; tests override the single dimension they vary.
pub struct KeySpec {
    pub image_id: i64,
    pub catalog_epoch: u64,
    pub source_revision: u64,
    pub editor_session: u64,
    pub selection_generation: u64,
    pub edit_revision: u64,
    pub process_version: i64,
    pub renderer_version: u64,
    pub cache_version: u64,
    pub viewport_revision: u64,
    pub display_revision: u64,
    pub device_epoch: u64,
    pub viewport_px: (u32, u32),
    pub scale_milli: u32,
    pub quality: QualityTier,
}

impl Default for KeySpec {
    fn default() -> Self {
        Self {
            image_id: 0,
            catalog_epoch: 1,
            source_revision: 0,
            editor_session: 0,
            selection_generation: 0,
            edit_revision: 0,
            process_version: 1,
            renderer_version: 1,
            cache_version: 1,
            viewport_revision: 0,
            display_revision: 0,
            device_epoch: 1,
            viewport_px: (64, 48),
            scale_milli: 1000,
            quality: QualityTier::Interactive,
        }
    }
}

impl KeySpec {
    pub fn key(&self) -> FrameKey {
        FrameKey {
            identity: RenderIdentity {
                source: SourceIdentity {
                    catalog_epoch: CatalogEpoch(self.catalog_epoch),
                    image_id: ImageId(self.image_id),
                    source_revision: SourceRevision(self.source_revision),
                },
                editor_session: EditorSession(self.editor_session),
                selection_generation: SelectionGeneration(self.selection_generation),
                edit_revision: EditRevision(self.edit_revision),
                process_version: ProcessVersion(self.process_version),
                renderer_version: RendererVersion(self.renderer_version),
                cache_version: CacheVersion(self.cache_version),
                viewport_revision: ViewportRevision(self.viewport_revision),
                display_revision: DisplayRevision(self.display_revision),
                device_epoch: DeviceEpoch(self.device_epoch),
            },
            viewport_px: self.viewport_px,
            scale_milli: self.scale_milli,
            quality: self.quality,
        }
    }
}
