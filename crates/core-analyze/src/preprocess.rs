//! Shared image → CHW f32 tensor preprocessing.

use image::{imageops::FilterType, RgbImage};

/// Resize `img` to exactly `size`×`size` (bilinear, aspect-squashing — this is what the RT-DETR /
/// D-FINE and Florence-2 image processors do), then produce a planar CHW f32 buffer with per-channel
/// `(px/255 - mean) / std`. Pass `mean=[0;3], std=[1;3]` for plain `÷255` (D-FINE: `do_normalize=false`).
pub fn to_chw(img: &RgbImage, size: u32, mean: [f32; 3], std: [f32; 3]) -> Vec<f32> {
    let resized = image::imageops::resize(img, size, size, FilterType::Triangle);
    let plane = (size * size) as usize;
    let mut out = vec![0f32; 3 * plane];
    for (x, y, px) in resized.enumerate_pixels() {
        let i = (y * size + x) as usize;
        out[i] = (px[0] as f32 / 255.0 - mean[0]) / std[0];
        out[plane + i] = (px[1] as f32 / 255.0 - mean[1]) / std[1];
        out[2 * plane + i] = (px[2] as f32 / 255.0 - mean[2]) / std[2];
    }
    out
}

/// YOLOv5 letterbox preprocessing (MegaDetector): resize aspect-preserving to fit `size`×`size`, pad
/// to a square with gray `(114)`, `÷255`, RGB, planar CHW. Returns the buffer plus `(scale, pad_x,
/// pad_y)` (in letterboxed pixels) so detected boxes can be mapped back to the source image.
pub fn to_letterbox_chw(img: &RgbImage, size: u32) -> (Vec<f32>, f32, f32, f32) {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let scale = (size as f32 / w as f32).min(size as f32 / h as f32);
    let nw = ((w as f32 * scale).round() as u32).clamp(1, size);
    let nh = ((h as f32 * scale).round() as u32).clamp(1, size);
    let resized = image::imageops::resize(img, nw, nh, FilterType::Triangle);
    let (pad_x, pad_y) = ((size - nw) / 2, (size - nh) / 2);
    let plane = (size * size) as usize;
    let gray = 114.0 / 255.0;
    let mut out = vec![gray; 3 * plane];
    for y in 0..nh {
        for x in 0..nw {
            let px = resized.get_pixel(x, y);
            let i = ((y + pad_y) * size + (x + pad_x)) as usize;
            out[i] = px[0] as f32 / 255.0;
            out[plane + i] = px[1] as f32 / 255.0;
            out[2 * plane + i] = px[2] as f32 / 255.0;
        }
    }
    (out, scale, pad_x as f32, pad_y as f32)
}

/// CLIP-style preprocessing for MobileCLIP-S1: resize the shortest edge to `size` (aspect-preserving),
/// center-crop to `size`×`size`, `÷255`, RGB, planar CHW. MobileCLIP S0–S2 use identity normalization
/// (`do_normalize=false`), unlike OpenAI CLIP — see the `Xenova/mobileclip_s1` preprocessor config.
pub fn to_clip_chw(img: &RgbImage, size: u32) -> Vec<f32> {
    let (w, h) = (img.width().max(1), img.height().max(1));
    // Shortest-edge resize: scale so min(w,h) == size.
    let scale = size as f32 / w.min(h) as f32;
    let (rw, rh) = (
        ((w as f32 * scale).round() as u32).max(size),
        ((h as f32 * scale).round() as u32).max(size),
    );
    let resized = image::imageops::resize(img, rw, rh, FilterType::Triangle);
    // Center crop to size×size.
    let (ox, oy) = ((rw - size) / 2, (rh - size) / 2);
    let plane = (size * size) as usize;
    let mut out = vec![0f32; 3 * plane];
    for y in 0..size {
        for x in 0..size {
            let px = resized.get_pixel(ox + x, oy + y);
            let i = (y * size + x) as usize;
            out[i] = px[0] as f32 / 255.0;
            out[plane + i] = px[1] as f32 / 255.0;
            out[2 * plane + i] = px[2] as f32 / 255.0;
        }
    }
    out
}
