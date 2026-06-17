//! Decode → color-managed **linear** RGB f32 (the cached working buffer for the develop pipeline).
//!
//! Two decode paths share the same color math:
//! - [`develop_linear`] — full-resolution rawler `RawDevelop` (PPG demosaic). Used for export.
//! - [`develop_linear_preview`] — half-resolution **superpixel** debayer for standard RGB Bayer
//!   sensors. Skips the expensive full-res interpolation (each 2×2 quad → one RGB pixel), so a
//!   "Fit" preview decodes ~3-4× faster. Falls back to [`develop_linear`] for non-RGB-Bayer.
//!
//! Output is **linear, wide-gamut ProPhoto-primaries** RGB (no `SRgb` gamma), preserving scene
//! values above 1.0 (`clip_negative`) — ready for scene-linear adjustments (exposure, WB, etc.) on
//! the GPU, which converts ProPhoto→sRGB only at the display transition.

use crate::error::RawError;
use image::metadata::Orientation;
use image::{imageops::FilterType, DynamicImage, Rgb32FImage};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
use rawler::imgop::raw::clip_negative;
use rawler::imgop::sensor::bayer::superpixel::Superpixel3Channel;
use rawler::imgop::sensor::bayer::Demosaic;
use rawler::imgop::xyz::{Illuminant, XYZ_TO_PROFOTORGB_D50};
use rawler::pixarray::{Color2D, PixF32, RgbF32};
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::RawImage;

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

    /// Upright the buffer from its EXIF orientation (1–8), swapping width/height for the 90°/270°
    /// cases. Absent/unknown orientation (or `1`) returns `self` untouched. Applied in linear light
    /// on the CPU before GPU upload, so the develop pipeline, histogram and export all stay upright
    /// with correct aspect — and (unlike a shader uv-transform) the GPU bindings are left alone.
    pub fn oriented(self, orientation: Option<u16>) -> LinearImage {
        let Some(o) = orientation.and_then(|v| Orientation::from_exif(v as u8)) else {
            return self;
        };
        if o == Orientation::NoTransforms {
            return self;
        }
        let buf = Rgb32FImage::from_raw(self.width, self.height, self.data)
            .expect("linear buffer dims match data length");
        let mut img = DynamicImage::ImageRgb32F(buf);
        img.apply_orientation(o);
        let buf = img.into_rgb32f();
        LinearImage {
            width: buf.width(),
            height: buf.height(),
            data: buf.into_raw(),
        }
    }
}

/// Decode + demosaic + white-balance + color-matrix (NO sRGB gamma) → full-resolution linear RGB f32,
/// uprighted to its EXIF orientation. One decoder serves both the metadata (orientation) read and the
/// pixel decode.
pub fn develop_linear(src: &RawSource) -> Result<LinearImage, RawError> {
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    // `RawImage.orientation` is hardcoded to Normal in rawler 0.7.2, so read EXIF orientation here.
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);
    let raw = decoder.raw_image(src, &params, false).map_err(de)?;
    Ok(develop_linear_from(&raw)?.oriented(orientation))
}

/// As-shot white-balance coefficients `[r, g, b, g2]` from the camera (neutral `[1;4]` if absent).
/// Used as a model input for learned auto-white-balance / lighting normalization. One raw decode.
pub fn as_shot_wb(src: &RawSource) -> Result<[f32; 4], RawError> {
    let raw = rawler::decode(src, &RawDecodeParams::default()).map_err(de)?;
    Ok(wb_or_neutral(&raw))
}

/// Select the camera→XYZ color matrix (prefer the D65 illuminant), padded to `[[f32;3];4]`.
/// `None` when no matrix is present or it is malformed — callers fall back to the calibrated path.
fn cam_xyz2cam(raw: &RawImage) -> Option<[[f32; 3]; 4]> {
    let (_ill, color_matrix) = raw
        .color_matrix
        .iter()
        .find(|(ill, _)| **ill == Illuminant::D65)
        .or_else(|| raw.color_matrix.iter().next())?;
    if color_matrix.is_empty() || color_matrix.len() % 3 != 0 {
        return None;
    }
    let mut xyz2cam = [[0f32; 3]; 4];
    let comps = (color_matrix.len() / 3).min(4);
    for i in 0..comps {
        for j in 0..3 {
            xyz2cam[i][j] = color_matrix[i * 3 + j];
        }
    }
    Some(xyz2cam)
}

/// As-shot white-balance coeffs, or neutral when the camera reported none.
fn wb_or_neutral(raw: &RawImage) -> [f32; 4] {
    if raw.wb_coeffs[0].is_nan() {
        [1.0; 4]
    } else {
        raw.wb_coeffs
    }
}

/// Full-res linear develop from an already-decoded `RawImage`.
///
/// Standard RGB sensors with a usable color matrix take the **headroom-preserving** path: demosaic +
/// crop WITHOUT rawler's `Calibrate`, then our own camera→linear-ProPhoto map with `clip_negative` — so
/// scene values >1.0 survive into the GPU buffer (the develop shader's soft highlight rolloff then
/// uses that headroom). This shares `map_3ch_to_rgb` with the preview path, so export == preview.
/// 4-colour / monochrome / matrix-less sensors fall back to rawler's calibrated develop.
fn develop_linear_from(raw: &RawImage) -> Result<LinearImage, RawError> {
    if let Some(xyz2cam) = cam_xyz2cam(raw) {
        let dev = RawDevelop {
            steps: vec![
                ProcessingStep::Rescale,
                ProcessingStep::Demosaic,
                ProcessingStep::CropActiveArea,
                ProcessingStep::CropDefault,
            ],
        };
        if let Intermediate::ThreeColor(px) = dev.develop_intermediate(raw).map_err(de)? {
            let wb = wb_or_neutral(raw);
            let rgb = map_3ch_to_rgb(&px, &wb, xyz2cam);
            return Ok(LinearImage {
                width: rgb.width as u32,
                height: rgb.height as u32,
                data: rgb.flatten(),
            });
        }
    }
    develop_calibrated(raw)
}

/// rawler's calibrated develop (clips highlights via `clip_euclidean_norm_avg`). Fallback for
/// non-RGB-Bayer sensors / images without a usable color matrix.
fn develop_calibrated(raw: &RawImage) -> Result<LinearImage, RawError> {
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
    let inter = dev.develop_intermediate(raw).map_err(de)?;
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
/// (black/white-level rescale, white-balance, camera→ProPhoto-linear matrix, recommended crop) mirrors
/// rawler's own `develop_intermediate` exactly, composed from its public helpers.
///
/// Anything that is not a standard RGB Bayer CFA (X-Trans, 4-colour CYGM, monochrome, linear-raw),
/// or any image whose color matrix is missing/malformed, transparently falls back to the
/// full-quality [`develop_linear`].
pub fn develop_linear_preview(src: &RawSource) -> Result<LinearImage, RawError> {
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    // EXIF orientation (rawler's `RawImage.orientation` is unreliable); applied to the result below.
    // Fallbacks to `develop_linear` are already uprighted there, so only the fast path applies it.
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);
    let mut raw = decoder.raw_image(src, &params, false).map_err(de)?;

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

    // Calibrate: camera→linear-ProPhoto via the shared helpers (same matrix selection + as-shot WB as the
    // full-res path); bail to the full decode if the matrix is missing/malformed.
    let Some(xyz2cam) = cam_xyz2cam(&raw) else {
        return develop_linear(src);
    };
    let wb = wb_or_neutral(&raw);
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
    }
    .oriented(orientation))
}

/// White-balance + camera→**linear ProPhoto** matrix map for 3-channel data. Shared by the preview
/// and full-res paths so they are pixel-identical.
///
/// Two deliberate departures from rawler's own `map_3ch_to_rgb` (which targets linear sRGB and is
/// `pub(crate)`): (1) the working space is **wide-gamut linear ProPhoto** ("Melissa RGB", what
/// Lightroom edits in) instead of sRGB/Rec.709 — so saturated camera colors are not gamut-clipped at
/// decode; the GPU develop converts ProPhoto→sRGB only at the display transition. (2) highlights are
/// clipped with `clip_negative` (floor negatives only, **keep values >1.0**) instead of
/// `clip_euclidean_norm_avg`, preserving scene-referred highlight headroom for the soft rolloff.
fn map_3ch_to_rgb(src: &Color2D<f32, 3>, wb_coeff: &[f32; 4], xyz2cam: [[f32; 3]; 4]) -> RgbF32 {
    // camera→linear-ProPhoto: ProPhoto→XYZ(D50) (rawler's XYZ→ProPhoto inverse) → cam, row-normalized
    // so camera neutral maps to ProPhoto neutral, then inverted to cam→ProPhoto.
    let pp_to_xyz = pseudo_inverse(XYZ_TO_PROFOTORGB_D50);
    let rgb2cam = normalize(multiply(&xyz2cam, &pp_to_xyz));
    let cam2rgb = pseudo_inverse(rgb2cam);

    let out: Vec<[f32; 3]> = src
        .pixels()
        .iter()
        .map(|pix| {
            let r = pix[0] * wb_coeff[0];
            let g = pix[1] * wb_coeff[1];
            let b = pix[2] * wb_coeff[2];
            let pp = [
                cam2rgb[0][0] * r + cam2rgb[0][1] * g + cam2rgb[0][2] * b,
                cam2rgb[1][0] * r + cam2rgb[1][1] * g + cam2rgb[1][2] * b,
                cam2rgb[2][0] * r + cam2rgb[2][1] * g + cam2rgb[2][2] * b,
            ];
            clip_negative(&pp)
        })
        .collect();

    Color2D::new_with(out, src.width, src.height)
}
