//! core-pipeline — GPU (wgpu/Metal) develop pipeline.
//!
//! Input: a cached linear, color-managed RGB f32 buffer (from `core_raw::develop_linear`).
//! GPU applies the interactive downstream modules (WB tweak, exposure, contrast, highlights/
//! shadows, saturation, display transform) and outputs RGBA8 for the webview canvas / export.

pub mod backend;
pub mod curve;
pub mod encode;
pub mod error;
pub mod histogram;
pub mod mask;
pub mod params;

pub use backend::{DevelopPipeline, GpuContext};
pub use curve::build_lut;
pub use encode::{rgba8_to_jpeg, rgba8_to_png};
pub use error::PipelineError;
pub use histogram::{histogram, histogram_from_jpeg, Histogram};
pub use params::{
    BrushStroke, ComponentKind, CurvePoint, DevelopParams, HslBand, LocalAdjust, Mask,
    MaskComponent, MaskOp, ToneCurve, MASK_CAP,
};

// Re-export the linear buffer type for convenience.
pub use core_raw::LinearImage;
