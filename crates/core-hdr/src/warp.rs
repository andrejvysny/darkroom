//! Inverse warp of a moving frame onto the reference frame's pixel grid, for hand-held HDR merge.
//!
//! `core_pano::align::estimate_alignment_rgb` gives a 3×3 (row-major) transform `M` mapping *moving*
//! pixels into the *reference* grid (`q ≈ M·p`). To resample we invert it once and, for each output
//! pixel `q` on the reference grid, read the moving frame at `p = M⁻¹·q` with bilinear interpolation.
//! Output pixels whose back-projection falls outside the moving frame get a **zero validity mask** so
//! the merge accumulator skips them entirely — without that, warp-border pixels (dark, out-of-frame)
//! would otherwise be pulled in at the `hat_weight` floor and paint dark halos along the seams.
//!
//! Pure f64 cofactor inversion is used deliberately: `core-hdr` stays a dependency-light math crate
//! and never links nalgebra (the transform arrives as a plain array from `core-pano`).

use core_raw::LinearImage;
use rayon::prelude::*;

/// Warp `mov` into the reference frame's `ref_w × ref_h` grid using the row-major transform `m`
/// (`q ≈ m·p`, `p` a pixel in `mov`). Returns `(rgb, mask)`:
/// - `rgb`: interleaved RGB, `3·ref_w·ref_h` f32; 0 where the sample is invalid.
/// - `mask`: `ref_w·ref_h` u8; `1` = the back-projected sample landed inside `mov` (contributes to
///   the merge), `0` = outside → the accumulator skips this pixel for this frame.
///
/// A singular `m` (or one whose inverse maps everything out of bounds) yields an all-zero mask.
pub fn warp_into_reference(
    mov: &LinearImage,
    ref_w: u32,
    ref_h: u32,
    m: &[[f64; 3]; 3],
) -> (Vec<f32>, Vec<u8>) {
    let rw = ref_w as usize;
    let rh = ref_h as usize;
    let mut rgb = vec![0.0f32; rw * rh * 3];
    let mut mask = vec![0u8; rw * rh];

    let Some(inv) = invert3(m) else {
        return (rgb, mask); // singular transform → nothing valid
    };
    let mw = mov.width as usize;
    let mh = mov.height as usize;

    // Row-parallel: each output row is independent. par_chunks_mut splits the interleaved RGB into
    // `rw*3`-float rows, zipped with the matching `rw`-byte mask rows.
    rgb.par_chunks_mut(rw * 3)
        .zip(mask.par_chunks_mut(rw))
        .enumerate()
        .for_each(|(y, (rgb_row, mask_row))| {
            let yf = y as f64;
            for x in 0..rw {
                let xf = x as f64;
                // p = inv · (x, y, 1), then homogeneous divide.
                let w = inv[2][0] * xf + inv[2][1] * yf + inv[2][2];
                if w.abs() < 1e-12 {
                    continue;
                }
                let sx = (inv[0][0] * xf + inv[0][1] * yf + inv[0][2]) / w;
                let sy = (inv[1][0] * xf + inv[1][1] * yf + inv[1][2]) / w;
                if let Some(px) = sample_bilinear(&mov.data, mw, mh, sx, sy) {
                    rgb_row[x * 3] = px[0];
                    rgb_row[x * 3 + 1] = px[1];
                    rgb_row[x * 3 + 2] = px[2];
                    mask_row[x] = 1;
                }
            }
        });

    (rgb, mask)
}

/// Bilinear sample of an interleaved-RGB buffer at `(sx, sy)`, or `None` if the point is outside the
/// pixel-centre grid `[0, w-1] × [0, h-1]`. In-bounds, the far-edge neighbour is clamped exactly as
/// `core-pano`'s `project::bilinear_rgb` does, so a sample on the last row/column stays valid.
#[inline]
fn sample_bilinear(data: &[f32], w: usize, h: usize, sx: f64, sy: f64) -> Option<[f32; 3]> {
    if sx < 0.0 || sy < 0.0 || sx > (w - 1) as f64 || sy > (h - 1) as f64 {
        return None;
    }
    let x0 = sx.floor() as usize;
    let y0 = sy.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = (sx - x0 as f64) as f32;
    let fy = (sy - y0 as f64) as f32;
    let mut out = [0.0f32; 3];
    for (ch, o) in out.iter_mut().enumerate() {
        let p00 = data[(y0 * w + x0) * 3 + ch];
        let p10 = data[(y0 * w + x1) * 3 + ch];
        let p01 = data[(y1 * w + x0) * 3 + ch];
        let p11 = data[(y1 * w + x1) * 3 + ch];
        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        *o = top + (bot - top) * fy;
    }
    Some(out)
}

/// Invert a 3×3 row-major matrix via the classical adjugate/determinant; `None` if (near-)singular.
fn invert3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);

    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if det.abs() < 1e-15 {
        return None;
    }
    let inv_det = 1.0 / det;
    Some([
        [
            (e * i - f * h) * inv_det,
            (c * h - b * i) * inv_det,
            (b * f - c * e) * inv_det,
        ],
        [
            (f * g - d * i) * inv_det,
            (a * i - c * g) * inv_det,
            (c * d - a * f) * inv_det,
        ],
        [
            (d * h - e * g) * inv_det,
            (b * g - a * h) * inv_det,
            (a * e - b * d) * inv_det,
        ],
    ])
}
