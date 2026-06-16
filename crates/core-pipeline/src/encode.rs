//! Encode rendered RGBA8 to PNG / JPEG (used by both develop-preview delivery and export).

use crate::error::PipelineError;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder};

pub fn rgba8_to_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, PipelineError> {
    let mut buf = Vec::new();
    PngEncoder::new(&mut buf).write_image(rgba, w, h, ExtendedColorType::Rgba8)?;
    Ok(buf)
}

pub fn rgba8_to_jpeg(rgba: &[u8], w: u32, h: u32, quality: u8) -> Result<Vec<u8>, PipelineError> {
    // JPEG is opaque RGB — drop alpha.
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[0..3]);
    }
    let mut buf = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(&rgb, w, h, ExtendedColorType::Rgb8)?;
    Ok(buf)
}
