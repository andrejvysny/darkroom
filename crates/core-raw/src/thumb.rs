//! Embedded-preview → thumbnail JPEG extraction.
//!
//! For Canon CR3 the embedded preview is full-resolution (e.g. 6960×4640), so we downscale it
//! to a grid-friendly edge and re-encode as JPEG. This is the demosaic-free Tier-0/1 path.

use crate::error::RawError;
use image::codecs::jpeg::JpegEncoder;
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType, GenericImageView};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

/// A generated thumbnail plus the source (full-image) dimensions.
pub struct Thumb {
    pub jpeg: Vec<u8>,
    /// NATIVE (pre-orientation, sensor-native) full-image dims. Kept stable for the capture
    /// fingerprint, which must not shift when orientation handling changes.
    pub src_width: u32,
    pub src_height: u32,
    /// ORIENTED (display) full-image dims — width/height after applying EXIF orientation, so a
    /// portrait shot reads as portrait. This is what the catalog stores for aspect/UI logic.
    pub disp_width: u32,
    pub disp_height: u32,
}

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

/// Decode the largest embedded preview to pixels (preview → full-image fallback chain).
pub fn preview_image(src: &RawSource) -> Result<DynamicImage, RawError> {
    if crate::display::is_display(src.path()) {
        return crate::display::decode_display_preview(&src.as_vec()?);
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    if let Some(img) = decoder.preview_image(src, &params).map_err(de)? {
        return Ok(img);
    }
    if let Some(img) = decoder.full_image(src, &params).map_err(de)? {
        return Ok(img);
    }
    Err(RawError::NoPreview)
}

/// Decode the embedded preview **once** and return the sensor-native pixels plus the EXIF
/// orientation (if any). A unified scan derives the native view (object detectors, which are
/// calibrated on sensor-native pixels) directly and the display view (faces) by applying the
/// orientation — so the JPEG is decoded a single time instead of twice. Mirrors [`preview_image`]'s
/// preview→full fallback chain so the native pixels are byte-identical to it.
pub fn preview_with_orientation(
    src: &RawSource,
) -> Result<(DynamicImage, Option<Orientation>), RawError> {
    if crate::display::is_display(src.path()) {
        return crate::display::decode_display_preview_native(&src.as_vec()?);
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    let img = match decoder.preview_image(src, &params).map_err(de)? {
        Some(img) => img,
        None => decoder
            .full_image(src, &params)
            .map_err(de)?
            .ok_or(RawError::NoPreview)?,
    };
    let orientation = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation)
        .and_then(|v| Orientation::from_exif(v as u8));
    Ok((img, orientation))
}

/// Embedded preview **uprighted to its EXIF orientation** — i.e. display space, matching what
/// [`thumbnail_jpeg`] serves (unlike [`preview_image`], which is sensor-native). Use this when boxes
/// derived from the pixels must line up with the displayed thumbnail (face detection / overlays).
pub fn oriented_preview(src: &RawSource) -> Result<DynamicImage, RawError> {
    let (mut img, orientation) = preview_with_orientation(src)?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

/// Extract the embedded preview, apply EXIF orientation, downscale so the longest edge ≤ `max_edge`,
/// encode JPEG at `quality`.
///
/// One decoder handles both the preview decode and the orientation read (the embedded preview is
/// sensor-native, so portraits arrive sideways until we upright them from the EXIF tag). The
/// returned `src_*` dims are the *native* preview dimensions (pre-orientation) so the capture
/// fingerprint stays stable across this change.
pub fn thumbnail_jpeg(src: &RawSource, max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
    if crate::display::is_display(src.path()) {
        let bytes = src.as_vec()?;
        let orientation = crate::display::exif_orientation(&bytes);
        return crate::display::decode_display_thumb(&bytes, orientation, max_edge, quality);
    }
    let decoder = rawler::get_decoder(src).map_err(de)?;
    let params = RawDecodeParams::default();
    let img = match decoder.preview_image(src, &params).map_err(de)? {
        Some(img) => img,
        None => decoder
            .full_image(src, &params)
            .map_err(de)?
            .ok_or(RawError::NoPreview)?,
    };
    let (w, h) = img.dimensions();

    // Upright the preview from its EXIF orientation (1–8). Absent/unknown → already upright.
    let mut img = img;
    if let Some(o) = decoder
        .raw_metadata(src, &params)
        .ok()
        .and_then(|md| md.exif.orientation)
        .and_then(|v| Orientation::from_exif(v as u8))
    {
        img.apply_orientation(o);
    }

    let (ow, oh) = img.dimensions();
    let scaled = if ow.max(oh) > max_edge {
        // `thumbnail` preserves aspect ratio, fitting within the box; fast triangle filter.
        img.thumbnail(max_edge, max_edge)
    } else {
        img
    };
    let rgb = scaled.to_rgb8();
    let mut buf = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(Thumb {
        jpeg: buf,
        src_width: w,
        src_height: h,
        disp_width: ow,
        disp_height: oh,
    })
}
