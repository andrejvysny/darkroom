//! Decode → color-managed **linear** RGB f32 (the cached working buffer for the develop pipeline).
//!
//! Uses rawler's `RawDevelop` with the `SRgb` (gamma) step omitted, so the output is linear,
//! sRGB-primaries RGB — ready for scene-linear adjustments (exposure, WB, etc.) on the GPU.

use crate::error::RawError;
use image::{imageops::FilterType, Rgb32FImage};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::rawsource::RawSource;

/// Interleaved linear RGB f32 image.
#[derive(Clone)]
pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

impl LinearImage {
    /// Downscale (in linear light) so the longest edge ≤ `max_edge`. Clones if already small enough.
    pub fn downscaled(&self, max_edge: u32) -> LinearImage {
        let longest = self.width.max(self.height);
        if longest <= max_edge {
            return self.clone();
        }
        let scale = max_edge as f32 / longest as f32;
        let nw = ((self.width as f32 * scale).round() as u32).max(1);
        let nh = ((self.height as f32 * scale).round() as u32).max(1);
        let buf = Rgb32FImage::from_raw(self.width, self.height, self.data.clone())
            .expect("linear buffer dims match data length");
        let resized = image::imageops::resize(&buf, nw, nh, FilterType::Lanczos3);
        LinearImage {
            width: nw,
            height: nh,
            data: resized.into_raw(),
        }
    }
}

/// Decode + demosaic + white-balance + color-matrix (NO sRGB gamma) → linear RGB f32.
pub fn develop_linear(src: &RawSource) -> Result<LinearImage, RawError> {
    let raw = rawler::decode(src, &RawDecodeParams::default()).map_err(de)?;
    let dev = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
            ProcessingStep::WhiteBalance,
            ProcessingStep::Calibrate,
            ProcessingStep::CropDefault,
        ],
    };
    let inter = dev.develop_intermediate(&raw).map_err(de)?;
    Ok(match inter {
        Intermediate::ThreeColor(px) => {
            let d = px.dim();
            LinearImage {
                width: d.w as u32,
                height: d.h as u32,
                data: px.flatten(),
            }
        }
        Intermediate::FourColor(px) => {
            let d = px.dim();
            let f = px.flatten();
            let mut data = Vec::with_capacity(d.w * d.h * 3);
            for c in f.chunks_exact(4) {
                data.extend_from_slice(&c[0..3]);
            }
            LinearImage {
                width: d.w as u32,
                height: d.h as u32,
                data,
            }
        }
        Intermediate::Monochrome(px) => {
            let d = px.dim();
            let mut data = Vec::with_capacity(d.w * d.h * 3);
            for v in &px.data {
                data.push(*v);
                data.push(*v);
                data.push(*v);
            }
            LinearImage {
                width: d.w as u32,
                height: d.h as u32,
                data,
            }
        }
    })
}
