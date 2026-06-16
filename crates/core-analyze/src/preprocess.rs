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
