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
use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
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
        // Defensive: a buffer whose length doesn't match its dims would panic in `from_raw`. That
        // invariant always holds for our decoders, but never crash on it — return the source
        // un-downscaled instead.
        let Some(buf) = Rgb32FImage::from_raw(self.width, self.height, self.data.clone()) else {
            return self.clone();
        };
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
        self.resize_consuming(max_edge, FilterType::Triangle)
    }

    /// Consuming **high-quality** (Lanczos3) downscale so the longest edge ≤ `max_edge`. Like
    /// [`Self::downscale_into`] but sharper — for *settled* outputs (canonical + edited thumbnails)
    /// that must match the GPU canvas, not throwaway fit-previews. Moves the buffer (no clone).
    pub fn downscale_into_hq(self, max_edge: u32) -> LinearImage {
        self.resize_consuming(max_edge, FilterType::Lanczos3)
    }

    /// Shared consuming downscale: move the backing buffer into a resize with `filter`. Returns
    /// `self` unchanged when already small enough or on a dims/length mismatch (never panics).
    fn resize_consuming(self, max_edge: u32, filter: FilterType) -> LinearImage {
        let longest = self.width.max(self.height);
        if longest <= max_edge {
            return self;
        }
        if self.data.len() != self.width as usize * self.height as usize * 3 {
            return self;
        }
        let scale = max_edge as f32 / longest as f32;
        let nw = ((self.width as f32 * scale).round() as u32).max(1);
        let nh = ((self.height as f32 * scale).round() as u32).max(1);
        let buf =
            Rgb32FImage::from_raw(self.width, self.height, self.data).expect("dims verified above");
        let resized = image::imageops::resize(&buf, nw, nh, filter);
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
        // Defensive: never panic on a dims/length mismatch (always holds for our decoders).
        if self.data.len() != self.width as usize * self.height as usize * 3 {
            return self;
        }
        let buf =
            Rgb32FImage::from_raw(self.width, self.height, self.data).expect("dims verified above");
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
    match crate::display::classify(src.path()) {
        crate::display::ImageKind::Jpeg | crate::display::ImageKind::Png => {
            let bytes = src.as_vec()?;
            let orientation = crate::display::exif_orientation(&bytes);
            return crate::display::decode_display_linear(&bytes, orientation);
        }
        crate::display::ImageKind::Heif => {
            return crate::heif::decode_heif_linear(&src.as_vec()?);
        }
        crate::display::ImageKind::Hdr => {
            return crate::hdr_file::read_hdr_linear(&src.as_vec()?);
        }
        crate::display::ImageKind::Raw => {}
    }
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

/// Plain-typed view of a decoded Bayer mosaic handed to a [`MosaicDenoiser`]. Carries everything a
/// raw-domain (pre-demosaic) denoiser needs — the mosaic samples, per-CFA-position black/white
/// levels, the CFA phase pattern, and capture ISO — WITHOUT exposing any rawler type, so the
/// ort-based denoiser can live in `core-analyze` while every rawler call stays in this crate.
pub struct MosaicInfo<'a> {
    /// `width * height` single-channel (cpp=1) mosaic samples, in the raw sensor domain (black
    /// level NOT yet subtracted — `black`/`white` describe that domain).
    pub data: &'a [u16],
    pub width: usize,
    pub height: usize,
    /// Black levels per CFA tile position, sensor scan order (see `cfa_pattern`).
    pub black: [f32; 4],
    /// White (saturation) levels per CFA tile position.
    pub white: [f32; 4],
    /// CFA tile dimensions (2×2 for standard Bayer).
    pub cfa_width: usize,
    pub cfa_height: usize,
    /// Row-major CFA color index per tile position (0=R, 1=G, 2=B), length `cfa_width * cfa_height`.
    pub cfa_pattern: Vec<u8>,
    /// Capture ISO for noise-level conditioning, if the file reported one.
    pub iso: Option<u32>,
}

/// A raw-domain (Bayer mosaic) denoiser. Implemented in `core-analyze` over ONNX Runtime and passed
/// in as a trait object, so `core-raw` (and its pinned rawler dependency) never links `ort`.
pub trait MosaicDenoiser {
    /// Denoise the mosaic in `info`, returning a NEW buffer of the SAME length and layout
    /// (`info.width * info.height`, cpp=1, identical CFA phase, same raw-value domain). A returned
    /// buffer of the wrong length is treated as a failure and the denoise is skipped.
    fn denoise(&self, info: &MosaicInfo) -> Vec<u16>;
}

/// Result of [`develop_linear_denoised`]: the ordinary clean develop, plus — only for supported RGB
/// Bayer sensors — the denoised develop. `denoised` is `None` for X-Trans / 4-colour / monochrome /
/// linear-raw / float-encoded / display images (the caller falls back to `clean`).
pub struct DenoiseOutput {
    pub clean: LinearImage,
    pub denoised: Option<LinearImage>,
}

/// Decode ONCE, then produce both the clean linear develop and — for supported RGB Bayer sensors — a
/// denoised develop. The denoised path runs `denoiser` over the raw mosaic, writes the result back
/// into the sensor buffer, and re-develops through the **unchanged** color pipeline (`develop_linear_from`):
/// only the mosaic samples differ, so demosaic, white-balance, camera→ProPhoto matrix, and every GPU
/// binding downstream are byte-for-byte the normal path. Both outputs are EXIF-uprighted.
pub fn develop_linear_denoised(
    src: &RawSource,
    denoiser: &dyn MosaicDenoiser,
) -> Result<DenoiseOutput, RawError> {
    // Only rawler-decoded RAW files have a mosaic; every other kind (display JPEG/PNG, HEIF,
    // merged-HDR EXR) returns the clean decode only.
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        return Ok(DenoiseOutput {
            clean: develop_linear(src)?,
            denoised: None,
        });
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    let md = decoder.raw_metadata(src, &params).ok();
    // `RawImage.orientation` is hardcoded Normal in rawler 0.7.2 — take it from EXIF (as elsewhere).
    let orientation = md.as_ref().and_then(|m| m.exif.orientation);
    let iso = md.as_ref().and_then(|m| {
        m.exif
            .iso_speed_ratings
            .map(|v| v as u32)
            .or(m.exif.iso_speed)
    });
    let mut raw = decoder.raw_image(src, &params, false).map_err(de)?;

    // Clean develop FIRST — the denoised path mutates the mosaic in place below.
    let clean = develop_linear_from(&raw)?.oriented(orientation);

    // Raw-domain denoise is only defined for standard RGB Bayer CFA with integer (cpp=1) samples.
    // Everything else (X-Trans, 4-colour, monochrome, linear-raw, float DNG) falls back to clean.
    let cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(c) if c.cfa.is_rgb() && !raw.is_monochrome() => {
            c.cfa.clone()
        }
        _ => {
            return Ok(DenoiseOutput {
                clean,
                denoised: None,
            })
        }
    };
    if raw.cpp != 1 || !matches!(raw.data, RawImageData::Integer(_)) {
        return Ok(DenoiseOutput {
            clean,
            denoised: None,
        });
    }

    // Scope the immutable mosaic borrow so it ends before the write-back below.
    let denoised_mosaic = {
        let info = MosaicInfo {
            data: raw.pixels_u16(),
            width: raw.width,
            height: raw.height,
            black: raw.blacklevel.as_bayer_array(),
            white: raw.whitelevel.as_bayer_array(),
            cfa_width: cfa.width,
            cfa_height: cfa.height,
            cfa_pattern: cfa.flat_pattern(),
            iso,
        };
        denoiser.denoise(&info)
    };

    // A denoiser returning the wrong length would panic in `copy_from_slice`; guard → fall back.
    if denoised_mosaic.len() != raw.pixels_u16().len() {
        return Ok(DenoiseOutput {
            clean,
            denoised: None,
        });
    }
    raw.pixels_u16_mut().copy_from_slice(&denoised_mosaic);
    let denoised = develop_linear_from(&raw)?.oriented(orientation);

    Ok(DenoiseOutput {
        clean,
        denoised: Some(denoised),
    })
}

/// As-shot white-balance coefficients `[r, g, b, g2]` from the camera (neutral `[1;4]` if absent).
/// Used as a model input for learned auto-white-balance / lighting normalization. One raw decode.
pub fn as_shot_wb(src: &RawSource) -> Result<[f32; 4], RawError> {
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        // Non-RAW images carry no camera white-balance coefficients; treat as neutral.
        return Ok([1.0; 4]);
    }
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
        // Deterministic fallback. `color_matrix` is a `HashMap`, whose `iter().next()` order is
        // unspecified and varies per instantiation — so a multi-illuminant body lacking a D65 entry
        // could yield a DIFFERENT matrix on the preview decode vs the export decode (two independent
        // decodes), silently breaking the documented `export == preview` invariant. Pick a stable
        // matrix via `min_by_key` over the (Ord) `Illuminant`.
        .or_else(|| raw.color_matrix.iter().min_by_key(|(ill, _)| **ill))?;
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
/// uses that headroom). This shares `map_3ch_to_rgb` with the preview path, so export matches the
/// preview's GLOBAL tone/color; pixel-local detail differs (the preview is a ½-res superpixel
/// demosaic, so fine colored detail is not bit-identical). 4-colour / monochrome / matrix-less
/// sensors fall back to rawler's calibrated develop.
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
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        // No superpixel fast path for a non-mosaic source; decode it directly.
        return develop_linear(src);
    }
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
    if let Some(crop) = raw.crop_area.or(raw.active_area) {
        let master = raw.active_area.unwrap_or(crop);
        // rawler's `Rect::adapt` ASSERTS the crop is contained in the master (origin ≥, size ≤).
        // For an unusual body whose crop_area isn't fully inside active_area that assert panics, and
        // this path is reachable from IPC on arbitrary files — bail to the full pipeline instead.
        let contained = crop.p.x >= master.p.x
            && crop.p.y >= master.p.y
            && crop.d.w <= master.d.w
            && crop.d.h <= master.d.h;
        if !contained {
            return develop_linear(src);
        }
        let mut crop = crop.adapt(&master);
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
