//! Embedded-preview → thumbnail JPEG extraction.
//!
//! For Canon CR3 the embedded preview is full-resolution (e.g. 6960×4640), so we downscale it
//! to a grid-friendly edge and re-encode as JPEG. This is the demosaic-free Tier-0/1 path.

use crate::error::RawError;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ExtendedColorType, GenericImageView};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;

/// A generated thumbnail plus the source (full-image) dimensions.
pub struct Thumb {
    pub jpeg: Vec<u8>,
    pub src_width: u32,
    pub src_height: u32,
}

fn de(e: impl std::fmt::Display) -> RawError {
    RawError::Decode(e.to_string())
}

/// Decode the largest embedded preview to pixels (preview → full-image fallback chain).
pub fn preview_image(src: &RawSource) -> Result<DynamicImage, RawError> {
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

/// Extract the embedded preview, downscale so the longest edge ≤ `max_edge`, encode JPEG at `quality`.
pub fn thumbnail_jpeg(src: &RawSource, max_edge: u32, quality: u8) -> Result<Thumb, RawError> {
    let img = preview_image(src)?;
    let (w, h) = img.dimensions();
    let scaled = if w.max(h) > max_edge {
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
    })
}
