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

/// Downscale a tightly-packed RGBA8 buffer so its longest edge ≤ `max_edge` (Lanczos3). Returns the
/// resized buffer + its new dims; returns the input unchanged when already within `max_edge` or on a
/// dims/length mismatch. Used to derive the small thumbnail from the larger preview render — one RAW
/// decode + one GPU render produce both tiers.
pub fn resize_rgba8(rgba: &[u8], w: u32, h: u32, max_edge: u32) -> (Vec<u8>, u32, u32) {
    let longest = w.max(h);
    if longest <= max_edge || w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
        return (rgba.to_vec(), w, h);
    }
    let scale = max_edge as f32 / longest as f32;
    let nw = ((w as f32 * scale).round() as u32).max(1);
    let nh = ((h as f32 * scale).round() as u32).max(1);
    let Some(img) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) else {
        return (rgba.to_vec(), w, h);
    };
    let resized = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Lanczos3);
    (resized.into_raw(), nw, nh)
}

/// Copy a sub-rectangle out of a tightly-packed RGBA8 buffer — a plain pixel copy, no resampling.
/// Used to extract the true-dimension crop from a full, letterbox-fit export render. Bounds are
/// assumed valid (`x + cw <= w`, `y + ch <= h`); callers use `Crop::export_rect` which guarantees it.
pub fn crop_rgba8(rgba: &[u8], w: u32, x: u32, y: u32, cw: u32, ch: u32) -> Vec<u8> {
    let row = (w * 4) as usize;
    let cwb = (cw * 4) as usize;
    let mut out = Vec::with_capacity(cwb * ch as usize);
    for ry in 0..ch as usize {
        let start = (y as usize + ry) * row + x as usize * 4;
        out.extend_from_slice(&rgba[start..start + cwb]);
    }
    out
}
