//! Decode → color-managed **linear** RGB f32 (the cached working buffer for the develop pipeline).
//!
//! Two decode paths share the same color math:
//! - [`develop_linear`] — full-resolution rawler `RawDevelop` (PPG demosaic). Used for export.
//! - [`develop_linear_preview`] — half-resolution **superpixel** debayer for standard RGB Bayer
//!   sensors. Skips the expensive full-res interpolation (each 2×2 quad → one RGB pixel), so a
//!   "Fit" preview decodes ~3-4× faster. Falls back to [`develop_linear`] for non-RGB-Bayer.
//!
//! Both omit the `SRgb` (gamma) step, so the output is linear, sRGB-primaries RGB — ready for
//! scene-linear adjustments (exposure, WB, etc.) on the GPU.

use crate::error::RawError;
use image::{imageops::FilterType, Rgb32FImage};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
use rawler::imgop::raw::clip_euclidean_norm_avg;
use rawler::imgop::sensor::bayer::superpixel::Superpixel3Channel;
use rawler::imgop::sensor::bayer::Demosaic;
use rawler::imgop::xyz::{Illuminant, SRGB_TO_XYZ_D65};
use rawler::pixarray::{Color2D, PixF32, RgbF32};
use rawler::rawimage::RawPhotometricInterpretation;
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
    /// High-quality Lanczos3; prefer [`Self::downscale_into`] in hot paths to avoid the clone.
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

    /// Consuming downscale so the longest edge ≤ `max_edge`. Moves the backing buffer into the
    /// resize (no 400 MB clone), and uses a Triangle filter — quality is irrelevant for a preview
    /// that is already binned + fit-to-screen, and it is markedly cheaper than Lanczos3.
    pub fn downscale_into(self, max_edge: u32) -> LinearImage {
        let longest = self.width.max(self.height);
        if longest <= max_edge {
            return self;
        }
        let scale = max_edge as f32 / longest as f32;
        let nw = ((self.width as f32 * scale).round() as u32).max(1);
        let nh = ((self.height as f32 * scale).round() as u32).max(1);
        let buf = Rgb32FImage::from_raw(self.width, self.height, self.data)
            .expect("linear buffer dims match data length");
        let resized = image::imageops::resize(&buf, nw, nh, FilterType::Triangle);
        LinearImage {
            width: nw,
            height: nh,
            data: resized.into_raw(),
        }
    }
}

/// Decode + demosaic + white-balance + color-matrix (NO sRGB gamma) → full-resolution linear RGB f32.
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

/// Fast **half-resolution** linear decode for the develop preview.
///
/// For standard RGB Bayer sensors this uses rawler's superpixel debayer (each 2×2 Bayer quad → one
/// real RGB pixel, output is ½×½), skipping the costly full-res PPG interpolation. The color math
/// (black/white-level rescale, white-balance, camera→sRGB-linear matrix, recommended crop) mirrors
/// rawler's own `develop_intermediate` exactly, composed from its public helpers.
///
/// Anything that is not a standard RGB Bayer CFA (X-Trans, 4-colour CYGM, monochrome, linear-raw),
/// or any image whose color matrix is missing/malformed, transparently falls back to the
/// full-quality [`develop_linear`].
pub fn develop_linear_preview(src: &RawSource) -> Result<LinearImage, RawError> {
    let mut raw = rawler::decode(src, &RawDecodeParams::default()).map_err(de)?;

    // Fast path only for standard 3-colour RGB Bayer; everything else uses the full pipeline.
    let is_rgb_bayer = matches!(
        &raw.photometric,
        RawPhotometricInterpretation::Cfa(c) if c.cfa.is_rgb()
    );
    if !is_rgb_bayer {
        return develop_linear(src);
    }

    // Rescale: apply black/white levels in-place → f32 in 0.0..1.0.
    raw.apply_scaling().map_err(de)?;
    let pixels = PixF32::new_with(raw.data.as_f32().into_owned(), raw.width, raw.height);

    // Demosaic via superpixel over the active area (ROI origin aligns the CFA pattern phase).
    let roi = raw.active_area.unwrap_or_else(|| pixels.rect());
    let demosaiced = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(config) => {
            Superpixel3Channel::new().demosaic(&pixels, &config.cfa, &config.colors, roi)
        }
        _ => unreachable!("guarded by is_rgb_bayer"),
    };

    // Calibrate: pick D65 (or first available) color matrix; bail to full decode if unusable.
    let matrix = raw
        .color_matrix
        .iter()
        .find(|(ill, _)| **ill == Illuminant::D65)
        .or_else(|| raw.color_matrix.iter().next());
    let Some((_ill, color_matrix)) = matrix else {
        return develop_linear(src);
    };
    if color_matrix.is_empty() || color_matrix.len() % 3 != 0 {
        return develop_linear(src);
    }
    let mut xyz2cam = [[0f32; 3]; 4];
    let comps = (color_matrix.len() / 3).min(4);
    for i in 0..comps {
        for j in 0..3 {
            xyz2cam[i][j] = color_matrix[i * 3 + j];
        }
    }
    // WhiteBalance: as-shot coeffs (or neutral if the camera reported none).
    let wb = if raw.wb_coeffs[0].is_nan() {
        [1.0; 4]
    } else {
        raw.wb_coeffs
    };
    let mut rgb = map_3ch_to_rgb(&demosaiced, &wb, xyz2cam);

    // CropDefault: trim to the recommended crop, made relative to the active area and halved to
    // match the superpixel (½-resolution) output — mirrors rawler's `develop_intermediate`.
    if let Some(mut crop) = raw.crop_area.or(raw.active_area) {
        crop = crop.adapt(&raw.active_area.unwrap_or(crop));
        if rgb.dim().w == roi.width() / 2 {
            crop.scale(0.5);
        }
        if crop.d != rgb.dim() {
            rgb = rgb.crop(crop);
        }
    }

    Ok(LinearImage {
        width: rgb.width as u32,
        height: rgb.height as u32,
        data: rgb.flatten(),
    })
}

/// White-balance + camera→sRGB-linear matrix map for 3-channel data.
///
/// Ported verbatim from rawler's `imgop::raw::map_3ch_to_rgb` (which is `pub(crate)`), composed
/// from rawler's public matrix/xyz helpers so the result is bit-for-bit equivalent to the
/// full-resolution path's `Calibrate` step.
fn map_3ch_to_rgb(src: &Color2D<f32, 3>, wb_coeff: &[f32; 4], xyz2cam: [[f32; 3]; 4]) -> RgbF32 {
    let rgb2cam = normalize(multiply(&xyz2cam, &SRGB_TO_XYZ_D65));
    let cam2rgb = pseudo_inverse(rgb2cam);

    let out: Vec<[f32; 3]> = src
        .pixels()
        .iter()
        .map(|pix| {
            let r = pix[0] * wb_coeff[0];
            let g = pix[1] * wb_coeff[1];
            let b = pix[2] * wb_coeff[2];
            let srgb = [
                cam2rgb[0][0] * r + cam2rgb[0][1] * g + cam2rgb[0][2] * b,
                cam2rgb[1][0] * r + cam2rgb[1][1] * g + cam2rgb[1][2] * b,
                cam2rgb[2][0] * r + cam2rgb[2][1] * g + cam2rgb[2][2] * b,
            ];
            clip_euclidean_norm_avg(&srgb)
        })
        .collect();

    Color2D::new_with(out, src.width, src.height)
}
