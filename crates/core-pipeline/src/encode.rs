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

/// Composite a solid-color border/frame around a tightly-packed RGBA8 image and return the framed
/// buffer + its new dims. `size_pct` is the border thickness as a percent of the long edge; `aspect`
/// (when `Some((aw, ah))`) pads the canvas out to that target ratio, centering the image (e.g. a
/// square 1:1 with a white border). Returns the input unchanged when the result would be the same
/// size (no margin + matching/absent aspect). The image is centered; margins are filled with `color`.
///
/// The geometry mirrors the frontend `computeFrame` so the live develop preview matches the export.
pub fn frame_rgba8(
    rgba: &[u8],
    w: u32,
    h: u32,
    color: [u8; 4],
    size_pct: f32,
    aspect: Option<(f32, f32)>,
) -> (Vec<u8>, u32, u32) {
    if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
        return (rgba.to_vec(), w, h);
    }
    let margin = ((size_pct.max(0.0) / 100.0) * w.max(h) as f32).round() as u32;
    let base_w = w + 2 * margin;
    let base_h = h + 2 * margin;
    let (ow, oh) = match aspect {
        Some((aw, ah)) if aw > 0.0 && ah > 0.0 => {
            let r = aw / ah;
            if (base_w as f32 / base_h as f32) < r {
                (((base_h as f32) * r).round() as u32, base_h)
            } else {
                (base_w, ((base_w as f32) / r).round() as u32)
            }
        }
        _ => (base_w, base_h),
    };
    if (ow, oh) == (w, h) {
        return (rgba.to_vec(), w, h);
    }
    let off_x = (ow - w) / 2;
    let off_y = (oh - h) / 2;

    // Fill the whole canvas with the border color, then blit the image rows into the centered rect.
    let mut out = Vec::with_capacity((ow * oh * 4) as usize);
    for _ in 0..(ow * oh) {
        out.extend_from_slice(&color);
    }
    let src_row = (w * 4) as usize;
    let dst_row = (ow * 4) as usize;
    for ry in 0..h as usize {
        let src = ry * src_row;
        let dst = (off_y as usize + ry) * dst_row + off_x as usize * 4;
        out[dst..dst + src_row].copy_from_slice(&rgba[src..src + src_row]);
    }
    (out, ow, oh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(buf: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    #[test]
    fn frame_uniform_margin_and_color() {
        // 100×50 red image, 10% border of the long edge (100) = 10px margin all sides → 120×70.
        let img = vec![255u8; 100 * 50 * 4]; // white-ish; set to red for a clear centre check
        let mut img = img;
        for c in img.chunks_exact_mut(4) {
            c.copy_from_slice(&[200, 0, 0, 255]);
        }
        let (out, ow, oh) = frame_rgba8(&img, 100, 50, [0, 0, 0, 255], 10.0, None);
        assert_eq!((ow, oh), (120, 70));
        assert_eq!(px(&out, ow, 0, 0), [0, 0, 0, 255], "corner is border color");
        assert_eq!(px(&out, ow, 60, 35), [200, 0, 0, 255], "centre is image");
    }

    #[test]
    fn frame_pads_to_square_aspect() {
        // 100×50 image, no margin, target 1:1 → pad width to 100 tall → but base is 100×50, r=1 so
        // oh = base_w/r = 100 → 100×100, image centered vertically (off_y = 25).
        let mut img = vec![0u8; 100 * 50 * 4];
        for c in img.chunks_exact_mut(4) {
            c.copy_from_slice(&[128, 128, 128, 255]);
        }
        let (out, ow, oh) = frame_rgba8(&img, 100, 50, [255, 255, 255, 255], 0.0, Some((1.0, 1.0)));
        assert_eq!((ow, oh), (100, 100));
        assert_eq!(
            px(&out, ow, 50, 0),
            [255, 255, 255, 255],
            "top strip is border"
        );
        assert_eq!(
            px(&out, ow, 50, 50),
            [128, 128, 128, 255],
            "centre is image"
        );
    }

    #[test]
    fn frame_noop_when_inactive() {
        let img = vec![7u8; 20 * 20 * 4];
        let (out, ow, oh) = frame_rgba8(&img, 20, 20, [0, 0, 0, 255], 0.0, None);
        assert_eq!((ow, oh), (20, 20));
        assert_eq!(out, img);
    }
}
