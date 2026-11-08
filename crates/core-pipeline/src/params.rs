//! GPU ABI and adapters. Persistent domain types are re-exported during migration.
pub use core_model::{
    AiPoint, Border, BrushStroke, CbRgb, ChannelMix, ComponentKind, Crop, CurvePoint, Denoise,
    DevelopParams, HslBand, LocalAdjust, Mask, MaskComponent, MaskOp, ToneCurve, ViewParams,
    HSL_BANDS, MASK_CAP,
};
mod adapter;
mod color;
mod geometry;
mod tone;
mod uniforms;
pub use adapter::{GpuParamsExt, GpuViewParamsExt};
pub(crate) use color::{wb_gain_from, wb_matrix};
pub use geometry::*;
pub use tone::*;
pub use uniforms::*;

/// A baked AI-mask alpha, supplied to the pipeline out-of-band (never serialized / never over IPC as
/// JSON) and stashed on the [`crate::backend::PreparedImage`] before a render. `alpha` is `width *
/// height` bytes (0..255), row-major, at the SAM working resolution (≤1024 longest side); the pre-pass
/// samples it by normalized UV (GPU-upscaled) and edge-refines it. Keyed to its `ComponentKind::Ai`
/// component by `hash`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AiCoverage {
    pub hash: String,
    pub width: u32,
    pub height: u32,
    pub alpha: Vec<u8>,
}
