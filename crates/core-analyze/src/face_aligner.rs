//! Face alignment — 2D similarity (Umeyama) warp of the 5 detected landmarks onto the canonical
//! ArcFace 112×112 reference, so the embedder always sees a pose-normalized crop.
//!
//! Mirrors InsightFace `face_align.norm_crop`: estimate a similarity transform (rotation + uniform
//! scale + translation, no shear) from the detected landmarks to `ARCFACE_DST`, then warp. The
//! estimate is Umeyama (SVD on the 2×2 cross-covariance) — pure Rust via `nalgebra`, no opencv.

use image::{Rgb, RgbImage};
use imageproc::geometric_transformations::{warp_into, Interpolation, Projection};
use nalgebra::{Matrix2, Vector2};

/// Canonical ArcFace 5-point reference for a 112×112 crop (InsightFace `arcface_dst`). Index order
/// matches SCRFD's landmark output order, so detected `kps[i]` pairs with `ARCFACE_DST[i]` directly.
pub const ARCFACE_DST: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

pub const ALIGN_SIZE: u32 = 112;

/// Estimate the 2×3 similarity transform `M` mapping `src` (detected landmarks, source-image pixels)
/// onto [`ARCFACE_DST`] via Umeyama: `dst ≈ M · [src; 1]`. Returned row-major `[[a,b,tx],[c,d,ty]]`.
pub fn umeyama(src: &[[f32; 2]; 5]) -> [[f32; 3]; 2] {
    let n = 5.0f32;
    let src_v: Vec<Vector2<f32>> = src.iter().map(|p| Vector2::new(p[0], p[1])).collect();
    let dst_v: Vec<Vector2<f32>> = ARCFACE_DST
        .iter()
        .map(|p| Vector2::new(p[0], p[1]))
        .collect();

    let mut mean_s = Vector2::zeros();
    let mut mean_d = Vector2::zeros();
    for i in 0..5 {
        mean_s += src_v[i];
        mean_d += dst_v[i];
    }
    mean_s /= n;
    mean_d /= n;

    // Cross-covariance Σ = (1/n) Σ (dst−μd)(src−μs)ᵀ, and the source variance.
    let mut cov = Matrix2::zeros();
    let mut var_s = 0.0f32;
    for i in 0..5 {
        let ds = src_v[i] - mean_s;
        let dd = dst_v[i] - mean_d;
        cov += dd * ds.transpose();
        var_s += ds.norm_squared();
    }
    cov /= n;
    var_s /= n;

    let svd = cov.svd(true, true);
    let (u, v_t, s) = (svd.u.unwrap(), svd.v_t.unwrap(), svd.singular_values);
    // Reflection correction: ensure a proper rotation (det = +1).
    let mut d = Vector2::new(1.0f32, 1.0);
    if u.determinant() * v_t.determinant() < 0.0 {
        d[1] = -1.0;
    }
    let r = u * Matrix2::from_diagonal(&d) * v_t;
    let scale = if var_s > 0.0 {
        (s[0] * d[0] + s[1] * d[1]) / var_s
    } else {
        1.0
    };
    let sr = r * scale;
    let t = mean_d - sr * mean_s;
    [
        [sr[(0, 0)], sr[(0, 1)], t[0]],
        [sr[(1, 0)], sr[(1, 1)], t[1]],
    ]
}

/// Warp `img` so the detected `kps` map onto the canonical reference, producing a 112×112 aligned RGB
/// face crop. Falls back to a plain center-resize if the transform is singular (degenerate landmarks).
pub fn align(img: &RgbImage, kps: &[[f32; 2]; 5]) -> RgbImage {
    let m = umeyama(kps);
    // `warp_into` takes the forward source→destination projection and inverts it internally to
    // resample, so we pass M (src→dst) directly (row-major 3×3, bottom row [0,0,1]).
    let proj = match Projection::from_matrix([
        m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], 0.0, 0.0, 1.0,
    ]) {
        Some(p) => p,
        None => {
            return image::imageops::resize(
                img,
                ALIGN_SIZE,
                ALIGN_SIZE,
                image::imageops::FilterType::Triangle,
            )
        }
    };
    let mut out = RgbImage::from_pixel(ALIGN_SIZE, ALIGN_SIZE, Rgb([0, 0, 0]));
    warp_into(
        img,
        &proj,
        Interpolation::Bilinear,
        Rgb([0, 0, 0]),
        &mut out,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An identity case: feeding the reference landmarks themselves must yield ~identity (scale≈1,
    /// rotation≈0, translation≈0).
    #[test]
    fn umeyama_identity() {
        let m = umeyama(&ARCFACE_DST);
        assert!((m[0][0] - 1.0).abs() < 1e-3, "a≈1, got {}", m[0][0]);
        assert!((m[1][1] - 1.0).abs() < 1e-3, "d≈1, got {}", m[1][1]);
        assert!(m[0][1].abs() < 1e-3 && m[1][0].abs() < 1e-3, "no rotation");
        assert!(
            m[0][2].abs() < 1e-2 && m[1][2].abs() < 1e-2,
            "no translation"
        );
    }

    /// A known similarity: scale the reference by 2 and translate by (10, 20). Umeyama must recover
    /// scale 2 and the translation (it maps src→dst where src is the transformed set).
    #[test]
    fn umeyama_recovers_scale_translation() {
        let mut src = ARCFACE_DST;
        for p in &mut src {
            p[0] = p[0] * 2.0 + 10.0;
            p[1] = p[1] * 2.0 + 20.0;
        }
        // src = 2·dst + t  ⇒  dst = 0.5·src − 0.5·t, so recovered scale ≈ 0.5.
        let m = umeyama(&src);
        let scale = (m[0][0] * m[0][0] + m[1][0] * m[1][0]).sqrt();
        assert!((scale - 0.5).abs() < 1e-3, "scale≈0.5, got {scale}");
        assert!(
            m[0][1].abs() < 1e-3 && m[1][0].abs() < 1e-3,
            "pure scale, no rotation"
        );
    }
}
