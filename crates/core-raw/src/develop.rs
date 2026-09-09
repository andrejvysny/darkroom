//! Decode → color-managed **linear** RGB f32 (the cached working buffer for the develop pipeline).
//!
//! Two decode paths share the same color math:
//! - [`develop_linear`] — full-resolution rawler `RawDevelop` (PPG demosaic). Used for export.
//! - [`develop_linear_preview`] — half-resolution **superpixel** debayer for standard RGB Bayer
//!   sensors. Skips the expensive full-res interpolation (each 2×2 quad → one RGB pixel), so a
//!   "Fit" preview decodes ~3-4× faster. Falls back to [`develop_linear`] for non-RGB-Bayer.
//!
//! Every public entry here runs inside [`crate::panic::catch_decode_panic`], and the sensor
//! layouts rawler would `todo!()` on are screened into typed [`RawError::Unsupported`] by
//! [`guard_developable`] before a decode ever starts — one odd file must never abort the app.
//!
//! Output is **linear, wide-gamut ProPhoto-primaries** RGB (no `SRgb` gamma), floored at zero but
//! never ceiling-clipped (`clip_negative`) — ready for scene-linear adjustments (exposure, WB, etc.)
//! on the GPU, which converts ProPhoto→sRGB only at the display transition.
//!
//! What sits above 1.0 is worth stating precisely, because 1.0 here is *sensor saturation*, not
//! "white". The mosaic itself cannot exceed 1.0, so all headroom is manufactured after it: by the
//! white-balance gains (a channel with gain 1.66 reads 1.66 once its photosites clip, and a
//! genuinely bright — unclipped — red or blue reads high for the same reason), and by the
//! camera→ProPhoto matrix. Pixels at saturation are additionally rewritten by
//! [`reconstruct_clipped`], which drops their chroma to zero at unchanged luma, so a blown
//! highlight develops NEUTRAL at `mean(gains)` — still above 1.0 for any real as-shot balance —
//! instead of carrying the as-shot cast. The excursion above 1.0 stays; only its colour changes.

use crate::error::RawError;
use crate::panic::catch_decode_panic;
use image::metadata::Orientation;
use image::{imageops::FilterType, DynamicImage, Rgb32FImage};
use rawler::decoders::RawDecodeParams;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::matrix::{multiply, normalize, pseudo_inverse};
use rawler::imgop::raw::clip_negative;
use rawler::imgop::sensor::bayer::superpixel::Superpixel3Channel;
use rawler::imgop::sensor::{Demosaic, SensorType};
use rawler::imgop::xyz::XYZ_TO_PROFOTORGB_D50;
use rawler::imgop::Rect;
use rawler::pixarray::{Color2D, PixF32, RgbF32};
use rawler::rawimage::{RawImageData, RawPhotometricInterpretation};
use rawler::rawsource::RawSource;
use rawler::RawImage;
use rayon::prelude::*;
use std::borrow::Cow;

/// Interleaved linear RGB f32 image.
#[derive(Clone)]
pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
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
    catch_decode_panic("develop_linear", || develop_linear_inner(src))
}

fn develop_linear_inner(src: &RawSource) -> Result<LinearImage, RawError> {
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
    let decoder = rawler::get_decoder(src)?;
    let params = RawDecodeParams::default();
    // `RawImage.orientation` is only populated by rawler's DNG decoder (Normal elsewhere), so read EXIF orientation here.
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);
    let raw = decoder.raw_image(src, &params, false)?;
    Ok(develop_linear_from(&raw)?.oriented(orientation))
}

/// [`develop_linear`] with an optional white-balance override, returning the WB actually applied.
///
/// The HDR merge decodes every bracket frame with the REFERENCE frame's as-shot WB (auto-WB can
/// drift between frames of a bracket, which would blend mismatched colors): decode the reference
/// with `None` (capturing its as-shot WB from the returned tuple), then each other frame with
/// `Some(ref_wb)`.
///
/// The override reaches only the headroom-preserving RGB-Bayer path (`map_3ch_to_rgb`); the
/// calibrated fallback (4-colour CYGM) bakes rawler's own WB and the monochrome path has no WB at
/// all — both report neutral `[1;4]`.
/// Non-RAW sources decode normally and report neutral. `develop_linear(src)` ≡
/// `develop_linear_wb(src, None).0` byte-for-byte.
pub fn develop_linear_wb(
    src: &RawSource,
    wb_override: Option<[f32; 4]>,
) -> Result<(LinearImage, [f32; 4]), RawError> {
    catch_decode_panic("develop_linear_wb", || {
        develop_linear_wb_inner(src, wb_override)
    })
}

fn develop_linear_wb_inner(
    src: &RawSource,
    wb_override: Option<[f32; 4]>,
) -> Result<(LinearImage, [f32; 4]), RawError> {
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        return Ok((develop_linear_inner(src)?, [1.0; 4]));
    }
    let decoder = rawler::get_decoder(src)?;
    let params = RawDecodeParams::default();
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);
    let raw = decoder.raw_image(src, &params, false)?;
    let (img, wb) = develop_linear_from_wb(&raw, wb_override)?;
    Ok((img.oriented(orientation), wb))
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
    catch_decode_panic("develop_linear_denoised", || {
        develop_linear_denoised_inner(src, denoiser)
    })
}

fn develop_linear_denoised_inner(
    src: &RawSource,
    denoiser: &dyn MosaicDenoiser,
) -> Result<DenoiseOutput, RawError> {
    // Only rawler-decoded RAW files have a mosaic; every other kind (display JPEG/PNG, HEIF,
    // merged-HDR EXR) returns the clean decode only.
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        return Ok(DenoiseOutput {
            clean: develop_linear_inner(src)?,
            denoised: None,
        });
    }
    let decoder = rawler::get_decoder(src)?;
    let params = RawDecodeParams::default();
    let md = decoder.raw_metadata(src, &params).ok();
    // `RawImage.orientation` is only populated by rawler's DNG decoder — take it from EXIF (as elsewhere).
    let orientation = md.as_ref().and_then(|m| m.exif.orientation);
    let iso = md.as_ref().and_then(|m| {
        m.exif
            .iso_speed_ratings
            .map(|v| v as u32)
            .or(m.exif.iso_speed)
    });
    let mut raw = decoder.raw_image(src, &params, false)?;

    // Clean develop FIRST — the denoised path mutates the mosaic in place below.
    let clean = develop_linear_from(&raw)?.oriented(orientation);

    // Raw-domain denoise is only defined for standard RGB Bayer CFA with integer (cpp=1) samples.
    // Everything else (X-Trans, 4-colour, monochrome, linear-raw, float DNG) falls back to clean.
    // `MosaicInfo` describes the mosaic with 4-entry black/white arrays and a 2×2 phase, so the
    // gate must be the same strict 2×2-Bayer test the superpixel preview uses — not just `is_rgb()`.
    let cfa = match &raw.photometric {
        RawPhotometricInterpretation::Cfa(c)
            if is_rgb_bayer(&raw.photometric) && !raw.is_monochrome() =>
        {
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
    catch_decode_panic("as_shot_wb", || as_shot_wb_inner(src))
}

fn as_shot_wb_inner(src: &RawSource) -> Result<[f32; 4], RawError> {
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        // Non-RAW images carry no camera white-balance coefficients; treat as neutral.
        return Ok([1.0; 4]);
    }
    let raw = rawler::decode(src, &RawDecodeParams::default())?;
    Ok(wb_or_neutral(&raw))
}

/// The camera→XYZ color matrix to develop `wb` with, padded to `[[f32;3];4]`. `None` when the file
/// carries no usable matrix — callers fall back to the calibrated path.
///
/// A dual-illuminant camera (Canon's R7 ships an A and a D65 matrix) gets the two **interpolated**
/// at the temperature of its own as-shot neutral, per the DNG spec; see
/// [`crate::color::select_cam_matrix`] for the math and for why the selection must never depend on
/// `HashMap` order. `wb` must be the balance the pixels are actually developed with, override
/// included, or the matrix would be picked for a white point the image does not have.
pub(crate) fn cam_xyz2cam(raw: &RawImage, wb: &[f32; 4]) -> Option<[[f32; 3]; 4]> {
    crate::color::select_cam_matrix(&raw.color_matrix, wb)
}

/// As-shot white-balance coeffs, or neutral when the camera reported none — or reported nonsense.
///
/// The RGB gains are multiplied straight into the pixels before the camera→ProPhoto matrix, so a
/// zero, negative, infinite or NaN coefficient silently poisons the entire buffer (a black or NaN
/// image, and NaNs propagate into the GPU texture and the histogram). Some third-party DNG writers
/// and damaged makernotes do produce those. `[1/16, 16]` is far wider than any real camera's
/// as-shot balance (extreme tungsten/underwater is ≈3-4×) while still rejecting garbage. The `E`
/// coefficient (`[3]`) is deliberately NOT checked: rawler stores `NaN` there for every 3-colour
/// sensor and `map_3ch_to_rgb` never reads it.
pub(crate) fn wb_or_neutral(raw: &RawImage) -> [f32; 4] {
    const MIN_GAIN: f32 = 1.0 / 16.0;
    const MAX_GAIN: f32 = 16.0;
    let sane = raw.wb_coeffs[0..3]
        .iter()
        .all(|c| c.is_finite() && *c > 0.0 && (MIN_GAIN..=MAX_GAIN).contains(c));
    if sane {
        raw.wb_coeffs
    } else {
        // Not an error: a missing WB (all-NaN) is the normal case for old/linear DNGs.
        log::warn!(
            "{} {}: implausible as-shot white balance {:?} — developing neutral",
            raw.clean_make,
            raw.clean_model,
            &raw.wb_coeffs[0..3]
        );
        [1.0; 4]
    }
}

/// Is this photometric a standard 3-colour **2×2** Bayer CFA — the only mosaic our fast paths and
/// the raw-domain denoiser can handle? `CFA::is_rgb()` alone is not enough: X-Trans is also "RGB",
/// but its 6×6 tile makes every 2×2-quad assumption (superpixel debayer, `MosaicInfo`'s 4-entry
/// black/white arrays) wrong, and `Superpixel3Channel` panics on it outright.
fn is_rgb_bayer(photometric: &RawPhotometricInterpretation) -> bool {
    matches!(
        photometric,
        RawPhotometricInterpretation::Cfa(c)
            if c.cfa.is_rgb() && c.sensor == SensorType::Bayer && c.cfa.width == 2 && c.cfa.height == 2
    )
}

/// Exactly the four conditions rawler's `Rect::adapt` ASSERTS (origin ≥ master's, size ≤ master's),
/// so every caller about to `adapt` — or to hand a crop to rawler's `CropDefault` step, which
/// adapts internally — must test this first; a violating file would otherwise abort the process
/// from inside a decode.
///
/// NOTE this is `adapt`'s precondition, NOT true containment: it does not check
/// `origin + size <= master edge`. Code that indexes a buffer with the rect needs its own bounds
/// test (see [`develop_mono`]).
pub(crate) fn crop_within(crop: Rect, master: Rect) -> bool {
    crop.p.x >= master.p.x
        && crop.p.y >= master.p.y
        && crop.d.w <= master.d.w
        && crop.d.h <= master.d.h
}

/// Drop a recommended crop that is NOT inside the active area, so rawler's `CropDefault` step
/// cannot trip `Rect::adapt`'s assert. Clearing (rather than erroring) keeps such a file usable —
/// the active-area crop still applies, the picture is just slightly larger than the camera
/// suggested. Borrows in the normal case, so the healthy path never pays for the clone.
fn without_bad_crop(raw: &RawImage) -> Cow<'_, RawImage> {
    match (raw.crop_area, raw.active_area) {
        (Some(crop), Some(master)) if !crop_within(crop, master) => {
            log::warn!(
                "{} {}: recommended crop {:?} is not inside the active area {:?} — ignoring it",
                raw.clean_make,
                raw.clean_model,
                crop,
                master
            );
            let mut owned = raw.clone();
            owned.crop_area = None;
            Cow::Owned(owned)
        }
        _ => Cow::Borrowed(raw),
    }
}

/// Refuse — with a typed, camera-tagged error — the sensor layouts that would make rawler panic
/// rather than return `Err`.
///
/// rawler's `develop_intermediate` ends in `todo!()` for any CFA that is neither RGB-Bayer,
/// 4-colour-Bayer nor X-Trans, and for `cpp` outside `{1, 3, 4}`; its `apply_scaling()` is
/// `todo!()` for `BlackIsZero`; `Superpixel3Channel` panics on any non-2×2 tile. Those are
/// reachable from IPC on arbitrary user files, so they are screened here into
/// [`RawError::Unsupported`] instead. X-Trans is included deliberately: rawler 0.8.0 does ship a
/// bilinear X-Trans demosaic, but Darkroom has never validated its color against a real Fuji file,
/// so it stays behind this gate rather than shipping unverified color.
///
/// Callers that CAN handle a case must screen it before calling: [`develop_linear_from_wb`] routes
/// monochrome to [`develop_mono`] first. Everything else — the panorama decode, the calibrated
/// fallback — gets a clean typed error here instead of an unwind out of rawler.
pub(crate) fn guard_developable(raw: &RawImage) -> Result<(), RawError> {
    let unsupported = |detail: String| RawError::Unsupported {
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        mode: String::new(),
        detail,
    };
    if raw.is_monochrome() {
        return Err(unsupported(
            "monochrome sensor (rawler cannot rescale BlackIsZero data)".to_string(),
        ));
    }
    if let RawPhotometricInterpretation::Cfa(c) = &raw.photometric {
        if c.sensor != SensorType::Bayer || c.cfa.width != 2 || c.cfa.height != 2 {
            return Err(unsupported(format!(
                "non-Bayer CFA {} (X-Trans is not supported yet)",
                c.cfa
            )));
        }
    }
    if raw.cpp != 1 && raw.cpp != 3 {
        return Err(unsupported(format!(
            "unsupported components per pixel {}",
            raw.cpp
        )));
    }
    Ok(())
}

/// Develop a monochrome sensor (`PhotometricInterpretation = BlackIsZero`) to linear RGB.
///
/// This exists because rawler cannot: `RawImage::apply_scaling()` is `todo!()` for `BlackIsZero`,
/// so every rawler develop path panics on a mono file before it produces a pixel. The math is the
/// same normalization rawler applies to a CFA — `(v - black) / (white - black)` — with no demosaic
/// (there is no mosaic) and no color matrix (there is no color), then the single channel is
/// replicated to R=G=B so the rest of the pipeline sees an ordinary linear image.
fn develop_mono(raw: &RawImage) -> Result<LinearImage, RawError> {
    let unsupported = |detail: String| RawError::Unsupported {
        make: raw.clean_make.clone(),
        model: raw.clean_model.clone(),
        mode: String::new(),
        detail,
    };
    if raw.cpp != 1 {
        return Err(unsupported(format!(
            "monochrome sensor with {} components per pixel",
            raw.cpp
        )));
    }
    let samples = raw.data.as_f32();
    if samples.len() != raw.width * raw.height {
        return Err(unsupported(format!(
            "monochrome buffer is {} samples but {}x{} expected",
            samples.len(),
            raw.width,
            raw.height
        )));
    }
    // Mono levels are scalars; `as_bayer_array` replicates them, so slot 0 is the whole story.
    let black = raw.blacklevel.as_bayer_array()[0];
    let white = raw.whitelevel.as_bayer_array()[0];
    let range = white - black;
    if !range.is_finite() || range <= 0.0 {
        return Err(unsupported(format!(
            "monochrome black level {black} is not below the white level {white}"
        )));
    }

    // Crop to the recommended (else active) area with a plain rect copy. rawler's `Rect::adapt` is
    // deliberately avoided: it asserts, and this path exists precisely to survive files whose
    // geometry tags are odd. The test below is STRICTER than `crop_within` on purpose — the rect
    // indexes `samples` directly, so `origin + size` must land inside the buffer, which
    // `Rect::adapt`'s preconditions alone do not guarantee.
    let full = Rect::new(
        rawler::imgop::Point::new(0, 0),
        rawler::imgop::Dim2::new(raw.width, raw.height),
    );
    let area = raw.crop_area.or(raw.active_area).unwrap_or(full);
    let fits = !area.is_empty()
        && area.p.x.saturating_add(area.d.w) <= raw.width
        && area.p.y.saturating_add(area.d.h) <= raw.height;
    let area = if fits {
        area
    } else {
        log::warn!(
            "{} {}: monochrome crop {:?} is outside the sensor {:?} — using the full frame",
            raw.clean_make,
            raw.clean_model,
            area,
            full
        );
        full
    };

    let mut data = Vec::with_capacity(area.d.w * area.d.h * 3);
    for y in area.p.y..area.p.y + area.d.h {
        let row = &samples[y * raw.width..y * raw.width + raw.width];
        for &v in &row[area.p.x..area.p.x + area.d.w] {
            let n = ((v - black) / range).max(0.0);
            data.extend_from_slice(&[n, n, n]);
        }
    }
    Ok(LinearImage {
        width: area.d.w as u32,
        height: area.d.h as u32,
        data,
    })
}

/// Full-res linear develop from an already-decoded `RawImage`.
///
/// Standard RGB sensors with a usable color matrix take the **headroom-preserving** path: demosaic +
/// crop WITHOUT rawler's `Calibrate`, then our own camera→linear-ProPhoto map with `clip_negative` — so
/// scene values >1.0 survive into the GPU buffer (the develop shader's soft highlight rolloff then
/// uses that headroom). This shares `map_3ch_to_rgb` with the preview path, so export matches the
/// preview's GLOBAL tone/color; pixel-local detail differs (the preview is a ½-res superpixel
/// demosaic, so fine colored detail is not bit-identical). 4-colour / matrix-less sensors fall back
/// to rawler's calibrated develop; monochrome takes [`develop_mono`] (rawler cannot develop it at
/// all), and anything rawler would panic on is refused by [`guard_developable`].
fn develop_linear_from(raw: &RawImage) -> Result<LinearImage, RawError> {
    Ok(develop_linear_from_wb(raw, None)?.0)
}

/// [`develop_linear_from`] with an optional WB override; returns the WB coefficients actually
/// applied (neutral for the calibrated fallback, where rawler bakes its own WB).
fn develop_linear_from_wb(
    raw: &RawImage,
    wb_override: Option<[f32; 4]>,
) -> Result<(LinearImage, [f32; 4]), RawError> {
    // Monochrome is screened FIRST: it is developable (by us, not by rawler — see `develop_mono`)
    // and there is no white balance to apply or report, so it never reaches the guard below.
    if raw.is_monochrome() {
        return Ok((develop_mono(raw)?, [1.0; 4]));
    }
    guard_developable(raw)?;
    // WB is resolved BEFORE the matrix: the dual-illuminant selection is a function of the balance
    // actually applied, so an HDR bracket decoded with the reference frame's WB override also
    // shares its colour matrix.
    let wb = wb_override.unwrap_or_else(|| wb_or_neutral(raw));
    if let Some(xyz2cam) = cam_xyz2cam(raw, &wb) {
        if let Some(px) = demosaic_camera_native(raw)? {
            let rgb = map_3ch_to_rgb(&px, &wb, xyz2cam);
            return Ok((
                LinearImage {
                    width: rgb.width as u32,
                    height: rgb.height as u32,
                    data: rgb.flatten(),
                },
                wb,
            ));
        }
    }
    Ok((develop_calibrated(raw)?, [1.0; 4]))
}

/// Demosaic to **camera-native** normalized 3-channel data: black/white-level rescale + demosaic +
/// active/recommended crop — WITHOUT white balance and WITHOUT any color matrix. This is the shared
/// front half of [`develop_linear_from`], split out so the panorama pipeline (`crate::pano`) can
/// stitch in the camera's own linear space (the merged LinearRaw DNG then re-applies the identical
/// wb+matrix math on decode). `None` for sensors that don't demosaic to three channels
/// (4-colour CYGM, monochrome) — those can't author a cpp=3 LinearRaw DNG.
pub(crate) fn demosaic_camera_native(raw: &RawImage) -> Result<Option<Color2D<f32, 3>>, RawError> {
    guard_developable(raw)?;
    let raw = without_bad_crop(raw);
    let dev = RawDevelop {
        steps: vec![
            ProcessingStep::Rescale,
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
            ProcessingStep::CropDefault,
        ],
    };
    Ok(match dev.develop_intermediate(&raw)? {
        Intermediate::ThreeColor(px) => Some(px),
        _ => None,
    })
}

/// rawler's calibrated develop (clips highlights via `clip_euclidean_norm_avg`). Fallback for
/// non-RGB-Bayer sensors / images without a usable color matrix.
fn develop_calibrated(raw: &RawImage) -> Result<LinearImage, RawError> {
    guard_developable(raw)?;
    let raw = without_bad_crop(raw);
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
    let inter = dev.develop_intermediate(&raw)?;
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
    catch_decode_panic("develop_linear_preview", || {
        develop_linear_preview_inner(src)
    })
}

fn develop_linear_preview_inner(src: &RawSource) -> Result<LinearImage, RawError> {
    if crate::display::classify(src.path()) != crate::display::ImageKind::Raw {
        // No superpixel fast path for a non-mosaic source; decode it directly.
        return develop_linear_inner(src);
    }
    let decoder = rawler::get_decoder(src)?;
    let params = RawDecodeParams::default();
    // EXIF orientation (rawler's `RawImage.orientation` is unreliable); applied to the result below.
    // Fallbacks to `develop_linear` are already uprighted there, so only the fast path applies it.
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation);
    let mut raw = decoder.raw_image(src, &params, false)?;

    // Fast path only for standard 3-colour 2×2 RGB Bayer. The sensor/dimension half of the test is
    // load-bearing: `Superpixel3Channel` indexes a 2×2 quad unconditionally and PANICS on any other
    // pattern (X-Trans is 6×6), so `is_rgb()` alone is not a sufficient guard.
    if !is_rgb_bayer(&raw.photometric) {
        return develop_linear_inner(src);
    }

    // Rescale: apply black/white levels in-place → f32 in 0.0..1.0.
    raw.apply_scaling()?;
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
    let wb = wb_or_neutral(&raw);
    let Some(xyz2cam) = cam_xyz2cam(&raw, &wb) else {
        return develop_linear_inner(src);
    };
    let mut rgb = map_3ch_to_rgb(&demosaiced, &wb, xyz2cam);

    // CropDefault: trim to the recommended crop, made relative to the active area and halved to
    // match the superpixel (½-resolution) output — mirrors rawler's `develop_intermediate`.
    if let Some(crop) = raw.crop_area.or(raw.active_area) {
        let master = raw.active_area.unwrap_or(crop);
        // rawler's `Rect::adapt` ASSERTS the crop is contained in the master (see [`crop_within`]).
        // For an unusual body whose crop_area isn't fully inside active_area that assert panics, and
        // this path is reachable from IPC on arbitrary files — bail to the full pipeline instead.
        if !crop_within(crop, master) {
            return develop_linear_inner(src);
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

/// Blend window for highlight reconstruction, in camera-native units where 1.0 is sensor
/// saturation. Below [`HL_LO`] a pixel passes through untouched (bit-for-bit the plain
/// white-balance product); from [`HL_HI`] up the reconstruction is fully applied. The window opens
/// below 1.0 deliberately: per-CFA-position white levels, black-level subtraction and the demosaic's
/// own interpolation smear the true clip point by a couple of percent, so a hard test at 1.0 would
/// leave a tinted fringe of half-clipped pixels ringing every blown highlight.
const HL_LO: f32 = 0.92;
const HL_HI: f32 = 0.995;

/// dcraw's `blend_highlights` basis: row 0 is luma (the plain channel sum), rows 1-2 are two
/// orthogonal chroma axes. Reconstruction shrinks the chroma rows and leaves luma alone.
const HL_TRANS: [[f32; 3]; 3] = [
    [1.0, 1.0, 1.0],
    [1.7320508, -1.7320508, 0.0],
    [-1.0, -1.0, 2.0],
];

/// dcraw's `itrans`, which is **3·[`HL_TRANS`]⁻¹** — dcraw divides the result by `colors` (3), and
/// so does [`reconstruct_clipped`]. `highlight_basis_is_a_true_inverse` asserts the
/// `HL_ITRANS · HL_TRANS = 3·I` identity that makes the round trip exact.
const HL_ITRANS: [[f32; 3]; 3] = [
    [1.0, 0.8660254, -0.5],
    [1.0, -0.8660254, -0.5],
    [1.0, 0.0, 1.0],
];

#[inline]
fn mat3_apply(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

/// Hermite smoothstep: 0 at or below `lo`, 1 at or above `hi`, C¹ in between.
#[inline]
fn smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// White-balance one camera-native pixel, reconstructing the colour of a clipped highlight.
///
/// A saturated photosite stops reporting how much light reached it, so a blown highlight arrives
/// with every channel pinned at the sensor's white level — numerically neutral. The as-shot gains
/// then turn that neutral clip into a cast: an EOS R7's `[1.77, 1.0, 1.66]` paints blown skies
/// magenta. dcraw's `blend_highlights` is the classic remedy and this is it: express the
/// white-balanced pixel in the luma+2-chroma basis [`HL_TRANS`], do the same for the pixel's
/// "clipped version" (every channel capped at the neutral ceiling `min(gains)`), then scale the two
/// chroma coordinates down to the clipped version's magnitude while leaving luma alone. A fully
/// saturated pixel's clipped version is exactly neutral, so its chroma goes to zero and it develops
/// white at luma `mean(gains)`. That mean is ≥ 1.0 unless BOTH the red and blue gains sit below
/// unity — which no real as-shot balance does, green being the most sensitive channel — so a
/// reconstructed clip still lands outside the HDR merge's "≥ 0.9 means clipped, weight it zero"
/// window and bracket merges keep discarding blown samples.
///
/// Two deliberate departures from dcraw: the hard `> clip` test becomes a smoothstep over
/// [`HL_LO`]..[`HL_HI`] so recovery fades in instead of banding at the clip boundary, and the
/// trigger is the **pre-gain** channel max, so the window means "this photosite is at saturation"
/// identically for all three channels rather than firing earliest on the highest-gain one.
///
/// Below [`HL_LO`] the result is the plain `pixel * gain` product, bit-for-bit.
fn reconstruct_clipped(px: [f32; 3], wb: &[f32; 4]) -> [f32; 3] {
    let c = [px[0] * wb[0], px[1] * wb[1], px[2] * wb[2]];
    let w = smoothstep(HL_LO, HL_HI, px[0].max(px[1]).max(px[2]));
    if w <= 0.0 {
        return c;
    }
    // The neutral ceiling: above `min(gain)` a channel can only be there because its own gain
    // stretched a clipped photosite, so that is where the reference "what a neutral clip would have
    // looked like" pixel is capped.
    let clip = wb[0].min(wb[1]).min(wb[2]);
    let cl = [c[0].min(clip), c[1].min(clip), c[2].min(clip)];
    let lab = mat3_apply(&HL_TRANS, c);
    let lab_cl = mat3_apply(&HL_TRANS, cl);
    // `max` guards the already-neutral pixel: its chroma is zero in BOTH bases, and a 0/0 ratio
    // would poison the blend with NaN. With the floor the ratio is 0 and the blend is inert
    // (`lab[1..]` are zero anyway).
    let chroma_sq = (lab[1] * lab[1] + lab[2] * lab[2]).max(1e-12);
    let ratio = ((lab_cl[1] * lab_cl[1] + lab_cl[2] * lab_cl[2]) / chroma_sq).sqrt();
    let shrunk = mat3_apply(&HL_ITRANS, [lab[0], lab[1] * ratio, lab[2] * ratio]);
    [
        c[0] + (shrunk[0] / 3.0 - c[0]) * w,
        c[1] + (shrunk[1] / 3.0 - c[1]) * w,
        c[2] + (shrunk[2] / 3.0 - c[2]) * w,
    ]
}

/// White-balance (with clipped-highlight reconstruction) + camera→**linear ProPhoto** matrix map for
/// 3-channel data. Shared by the preview and full-res paths so they are pixel-identical.
///
/// Three deliberate departures from rawler's own `map_3ch_to_rgb` (which targets linear sRGB and is
/// `pub(crate)`): (1) the working space is **wide-gamut linear ProPhoto** ("Melissa RGB", what
/// Lightroom edits in) instead of sRGB/Rec.709 — so saturated camera colors are not gamut-clipped at
/// decode; the GPU develop converts ProPhoto→sRGB only at the display transition. (2) highlights are
/// clipped with `clip_negative` (floor negatives only, **keep values >1.0**) instead of
/// `clip_euclidean_norm_avg`, preserving scene-referred highlight headroom for the soft rolloff.
/// (3) pixels at sensor saturation go through [`reconstruct_clipped`] first, so a blown highlight
/// develops neutral rather than tinted by the as-shot gains.
pub(crate) fn map_3ch_to_rgb(
    src: &Color2D<f32, 3>,
    wb_coeff: &[f32; 4],
    xyz2cam: [[f32; 3]; 4],
) -> RgbF32 {
    // camera→linear-ProPhoto: ProPhoto→XYZ(D50) (rawler's XYZ→ProPhoto inverse) → cam, row-normalized
    // so camera neutral maps to ProPhoto neutral, then inverted to cam→ProPhoto.
    let pp_to_xyz = pseudo_inverse(XYZ_TO_PROFOTORGB_D50);
    let rgb2cam = normalize(multiply(&xyz2cam, &pp_to_xyz));
    let cam2rgb = pseudo_inverse(rgb2cam);

    // 32 MP × (blend + 3×3) is the one genuinely hot loop in a full-res develop, and every pixel is
    // independent. `par_iter` over the slice is an INDEXED parallel iterator, so `collect` writes
    // each result at its own index and every pixel runs exactly the arithmetic it would serially —
    // the output is bit-identical to the sequential version regardless of how rayon splits it,
    // which `denoise_seam`'s bit-exact assertion depends on.
    let out: Vec<[f32; 3]> = src
        .pixels()
        .par_iter()
        .map(|pix| {
            let [r, g, b] = reconstruct_clipped(*pix, wb_coeff);
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

#[cfg(test)]
mod tests {
    use super::*;
    use rawler::cfa::PlaneColor;
    use rawler::decoders::Camera;
    use rawler::imgop::xyz::XYZ_TO_SRGB_D65;
    use rawler::imgop::{Dim2, Point};
    use rawler::pixarray::PixU16;
    use rawler::rawimage::{BlackLevel, CFAConfig, WhiteLevel};

    /// A minimal in-memory monochrome sensor. Built directly (not via `synth`) because the DNG
    /// writer normalizes geometry — the point here is to feed the develop path a rect no writer
    /// would ever produce.
    fn mono_raw(w: usize, h: usize, crop: Option<Rect>) -> RawImage {
        let mut cam = Camera::new();
        cam.make = "Darkroom".into();
        cam.model = "MonoUnit".into();
        cam.clean_make = cam.make.clone();
        cam.clean_model = cam.model.clone();
        let mut raw = RawImage::new(
            cam,
            PixU16::new_with(vec![2048_u16; w * h], w, h),
            1,
            [f32::NAN; 4],
            RawPhotometricInterpretation::BlackIsZero,
            Some(BlackLevel::new(&[0_u32], 1, 1, 1)),
            Some(WhiteLevel::new(vec![4095_u32])),
            false,
        );
        raw.crop_area = crop;
        raw
    }

    /// A crop whose ORIGIN + SIZE runs off the sensor satisfies `Rect::adapt`'s preconditions but
    /// would index past the sample buffer. `develop_mono` must fall back to the full frame instead
    /// of panicking — this is the exact hole `crop_within` alone does not cover.
    #[test]
    fn mono_crop_running_past_the_sensor_falls_back_to_full_frame() {
        let (w, h) = (16usize, 8usize);
        // Origin ≥ (0,0) and size ≤ the sensor, yet 8 + 16 > 16 horizontally.
        let bad = Rect::new(Point::new(8, 4), Dim2::new(16, 8));
        assert!(
            crop_within(bad, Rect::new(Point::new(0, 0), Dim2::new(w, h))),
            "the rect must pass `adapt`'s preconditions, or this test proves nothing"
        );

        let img = develop_mono(&mono_raw(w, h, Some(bad))).expect("mono develop");
        assert_eq!((img.width, img.height), (w as u32, h as u32));
        assert_eq!(img.data.len(), w * h * 3);
    }

    /// A sane sub-rect IS honoured (so the fallback above is not just "always full frame").
    #[test]
    fn mono_honours_a_valid_crop() {
        let good = Rect::new(Point::new(2, 1), Dim2::new(8, 4));
        let img = develop_mono(&mono_raw(16, 8, Some(good))).expect("mono develop");
        assert_eq!((img.width, img.height), (8, 4));
        // 2048 / 4095 with black 0.
        assert!(
            (img.data[0] - 2048.0 / 4095.0).abs() < 1e-6,
            "{}",
            img.data[0]
        );
    }

    /// Implausible as-shot gains must be replaced by neutral rather than poisoning the buffer.
    #[test]
    fn wb_or_neutral_rejects_garbage_gains() {
        let mut raw = mono_raw(2, 2, None);
        for bad in [
            [0.0, 1.0, 1.0, f32::NAN],
            [-2.0, 1.0, 1.0, f32::NAN],
            [f32::INFINITY, 1.0, 1.0, f32::NAN],
            [2.0, f32::NAN, 1.0, f32::NAN],
            [200.0, 1.0, 1.0, f32::NAN],
            [0.001, 1.0, 1.0, f32::NAN],
        ] {
            raw.wb_coeffs = bad;
            assert_eq!(wb_or_neutral(&raw), [1.0; 4], "{bad:?} should be refused");
        }
        // A real, extreme-but-plausible tungsten balance survives untouched.
        raw.wb_coeffs = [1.2, 1.0, 3.9, f32::NAN];
        let wb = wb_or_neutral(&raw);
        assert_eq!(wb[0..3], [1.2, 1.0, 3.9]);
    }

    /// The camera-native (panorama) route has no monochrome branch, so the guard must stop a mono
    /// sensor before rawler's `apply_scaling()` reaches its `todo!()` for `BlackIsZero`.
    #[test]
    fn camera_native_refuses_monochrome_instead_of_panicking() {
        let err = demosaic_camera_native(&mono_raw(8, 8, None))
            .err()
            .expect("a monochrome sensor must be refused by the camera-native path");
        assert_eq!(err.kind(), crate::error::FailureKind::Unsupported);
        assert!(err.user_detail().contains("monochrome"), "{err}");
    }

    /// `develop_intermediate` matches `cpp` against `{1, 3, 4}` and `todo!()`s otherwise, so an
    /// out-of-range `cpp` must be an error here. Uses a CFA photometric so the monochrome arm
    /// above does not short-circuit the check.
    #[test]
    fn guard_refuses_odd_cpp() {
        let mut raw = mono_raw(2, 2, None);
        raw.photometric = RawPhotometricInterpretation::Cfa(CFAConfig::new(
            &rawler::CFA::new("RGGB"),
            &PlaneColor::new("RGB"),
        ));
        assert!(
            guard_developable(&raw).is_ok(),
            "cpp=1 RGGB must be allowed"
        );

        raw.cpp = 2;
        let err = guard_developable(&raw).expect_err("cpp=2 must be refused");
        assert_eq!(err.kind(), crate::error::FailureKind::Unsupported);
        assert_eq!(err.camera(), Some(("Darkroom", "MonoUnit")));
        assert!(err.user_detail().contains("components per pixel"), "{err}");
    }

    // --- clipped-highlight reconstruction ---------------------------------------------------------

    /// Plausible as-shot gains: green-normalized, red and blue lifted (an EOS R7 sits near this).
    const TEST_WB: [f32; 4] = [2.0, 1.0, 1.5, f32::NAN];

    fn chroma(px: [f32; 3]) -> f32 {
        let mx = px[0].max(px[1]).max(px[2]);
        let mn = px[0].min(px[1]).min(px[2]);
        if mx <= 1e-6 {
            0.0
        } else {
            (mx - mn) / mx
        }
    }

    /// dcraw applies `itrans` and then divides by the channel count, which is only a round trip if
    /// `HL_ITRANS · HL_TRANS = 3·I`. Everything [`reconstruct_clipped`] promises — luma preserved,
    /// an untouched pixel reproduced exactly when the chroma ratio is 1 — rests on that identity.
    #[test]
    fn highlight_basis_is_a_true_inverse() {
        let mut product = [[0f32; 3]; 3];
        for (i, out_row) in product.iter_mut().enumerate() {
            for (j, cell) in out_row.iter_mut().enumerate() {
                *cell = HL_TRANS
                    .iter()
                    .enumerate()
                    .map(|(k, trans_row)| HL_ITRANS[i][k] * trans_row[j])
                    .sum();
            }
        }
        for (i, row) in product.iter().enumerate() {
            for (j, v) in row.iter().enumerate() {
                let want = if i == j { 3.0 } else { 0.0 };
                assert!(
                    (v - want).abs() < 1e-6,
                    "HL_ITRANS·HL_TRANS[{i}][{j}] = {v}, want {want}"
                );
            }
        }
    }

    /// A fully saturated photosite (every channel at the white level) must develop NEUTRAL, at the
    /// luma the gains imply. This is the whole point of the feature: without it the as-shot gains
    /// paint a blown sky magenta. `mean(gains) ≥ 1` also keeps such a pixel outside the HDR merge's
    /// weighting window, so bracket merges still discard clipped samples.
    #[test]
    fn saturated_pixel_reconstructs_neutral() {
        let out = reconstruct_clipped([1.0, 1.0, 1.0], &TEST_WB);
        assert!(chroma(out) < 1e-4, "clipped white developed as {out:?}");
        let mean = (TEST_WB[0] + TEST_WB[1] + TEST_WB[2]) / 3.0;
        for (c, v) in out.iter().enumerate() {
            assert!((v - mean).abs() < 1e-5, "channel {c} = {v}, want {mean}");
        }
        assert!(
            mean >= 1.0,
            "a neutral clip must not drop below sensor white"
        );
    }

    /// Below the window the function is exactly `pixel * gain` — bit-for-bit, not approximately, so
    /// nothing in the mid-tones moved when reconstruction landed.
    #[test]
    fn midtones_are_untouched_by_reconstruction() {
        for px in [
            [0.5, 0.5, 0.5],
            [0.0, 0.0, 0.0],
            [0.8, 0.3, 0.5],
            [0.1, HL_LO, 0.4],
        ] {
            let out = reconstruct_clipped(px, &TEST_WB);
            let plain = [px[0] * TEST_WB[0], px[1] * TEST_WB[1], px[2] * TEST_WB[2]];
            assert_eq!(
                out.map(f32::to_bits),
                plain.map(f32::to_bits),
                "{px:?} was modified below the reconstruction window"
            );
        }
    }

    /// A pixel clipped in ONE channel keeps its colour — it is a bright red, not a blown white —
    /// but with the chroma pulled back toward what a neutral clip would have looked like. Luma (the
    /// plain channel sum) is preserved exactly, which is what stops the blend from changing exposure.
    #[test]
    fn partially_clipped_pixel_keeps_colour_with_less_chroma() {
        let px = [1.0, 0.4, 0.3];
        let plain = [px[0] * TEST_WB[0], px[1] * TEST_WB[1], px[2] * TEST_WB[2]];
        let out = reconstruct_clipped(px, &TEST_WB);
        assert!(
            chroma(out) < chroma(plain) * 0.9,
            "chroma {} was not reduced from {}",
            chroma(out),
            chroma(plain)
        );
        assert!(
            chroma(out) > 0.05,
            "a one-channel clip must stay coloured, got {out:?}"
        );
        let (a, b) = (out.iter().sum::<f32>(), plain.iter().sum::<f32>());
        assert!((a - b).abs() < 1e-5, "luma moved: {a} vs {b}");
    }

    /// Neutral gains have nothing to correct: `min(gain) == gain`, so the clipped reference pixel IS
    /// the pixel and the blend is inert even at full saturation.
    #[test]
    fn neutral_gains_leave_a_clipped_pixel_alone() {
        let out = reconstruct_clipped([1.0, 1.0, 1.0], &[1.0, 1.0, 1.0, f32::NAN]);
        for v in out {
            assert!((v - 1.0).abs() < 1e-6, "{out:?}");
        }
    }

    /// End-to-end over the real mapping function: every pixel below the window must come out
    /// bit-identical to the pre-reconstruction math (WB multiply → cam→ProPhoto → `clip_negative`),
    /// which is recomputed inline here rather than trusted. Lives in the crate (not `tests/`) because
    /// it needs the same rawler matrix helpers `map_3ch_to_rgb` uses.
    #[test]
    fn unclipped_pixels_match_the_pre_reconstruction_math() {
        let mut xyz2cam = [[0f32; 3]; 4];
        for (row, src) in xyz2cam.iter_mut().zip(XYZ_TO_SRGB_D65.iter()) {
            *row = *src;
        }
        let unclipped: Vec<[f32; 3]> = vec![
            [0.8, 0.3, 0.5],
            [0.0, 0.0, 0.0],
            [0.5, 0.5, 0.5],
            [0.1, 0.919, 0.4],
            [HL_LO, 0.2, 0.02],
        ];
        let src = Color2D::new_with(unclipped.clone(), unclipped.len(), 1);
        let got = map_3ch_to_rgb(&src, &TEST_WB, xyz2cam).flatten();

        let pp_to_xyz = pseudo_inverse(XYZ_TO_PROFOTORGB_D50);
        let cam2rgb = pseudo_inverse(normalize(multiply(&xyz2cam, &pp_to_xyz)));
        let want: Vec<f32> = unclipped
            .iter()
            .flat_map(|pix| {
                let r = pix[0] * TEST_WB[0];
                let g = pix[1] * TEST_WB[1];
                let b = pix[2] * TEST_WB[2];
                clip_negative(&[
                    cam2rgb[0][0] * r + cam2rgb[0][1] * g + cam2rgb[0][2] * b,
                    cam2rgb[1][0] * r + cam2rgb[1][1] * g + cam2rgb[1][2] * b,
                    cam2rgb[2][0] * r + cam2rgb[2][1] * g + cam2rgb[2][2] * b,
                ])
            })
            .collect();
        assert_eq!(
            got.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            want.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            "an unclipped pixel changed colour"
        );

        // ...and the test is not vacuous: a saturated pixel through the SAME call does move.
        let clipped = Color2D::new_with(vec![[1.0f32, 1.0, 1.0]], 1, 1);
        let out = map_3ch_to_rgb(&clipped, &TEST_WB, xyz2cam).flatten();
        assert!(
            (out[0] - out[1]).abs() < 1e-4 && (out[1] - out[2]).abs() < 1e-4,
            "a clipped white must develop neutral, got {out:?}"
        );
    }
}
