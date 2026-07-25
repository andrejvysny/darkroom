//! Public two-image alignment for hand-held Merge-to-HDR brackets.
//!
//! Panorama registration ([`crate::register`]) solves a whole graph of frames on the sphere; HDR
//! merge needs something much narrower: given a *reference* frame and one *moving* frame of the same
//! (hand-held) scene, recover the single planar transform that lands the moving frame on the
//! reference's pixel grid, so `core-hdr` can warp it and merge pixel-aligned radiance.
//!
//! This reuses the exact P1 machinery — [`features::extract_at`] (FAST-9 + steered BRIEF-256),
//! [`matching::match_descriptors`] (Hamming kNN2 + Lowe ratio + mutual best), and the RANSAC in
//! [`ransac`] — but exposes a small, nalgebra-free surface: buffers in, a plain `[[f64;3];3]`
//! row-major transform out. The default model is **affine** (see [`ransac::verify_pair_affine`] for
//! why projective terms are the wrong choice for a small hand-held delta).

use nalgebra::{Matrix3, Point2};

use crate::features::{self, fixed_test_pairs};
use crate::matching;
use crate::ransac;

/// Global motion model to fit between the two frames.
#[derive(Clone, Copy)]
pub enum AlignModel {
    /// 6-DOF affine (translation + rotation + scale + shear). The default for HDR brackets: it
    /// captures every distortion a small hand-held pose change induces while staying well
    /// conditioned on textureless content.
    Affine,
    /// 8-DOF projective homography. Correct for a pure rotation about the optical centre, but its
    /// perspective terms can extrapolate wildly when inliers cluster — use only when the frames have
    /// genuine, well-distributed parallax-free perspective change.
    Homography,
}

/// Estimate the planar transform mapping `mov` pixel coordinates INTO `ref`'s full-res grid
/// (`q ≈ M·p`, with `p` a pixel in `mov`), from two interleaved-RGB buffers (`len == w*h*3`).
///
/// Both buffers are treated as full-res (`full == buffer` dims), so the returned transform is in the
/// reference's true pixel units and can be fed straight to `core_hdr::warp_into_reference`. Returns
/// `None` when the pair does not confidently overlap (too few matches, or the RANSAC + Brown&Lowe
/// gate rejects the fit) — the caller then merges the frame unaligned.
///
/// The result is a **row-major** `[[f64;3];3]`: `M[r][c]`, with the last row `[0,0,1]` for the affine
/// model. nalgebra is deliberately kept out of this signature so `core-hdr` need not link it.
pub fn estimate_alignment_rgb(
    ref_rgb: &[f32],
    ref_w: usize,
    ref_h: usize,
    mov_rgb: &[f32],
    mov_w: usize,
    mov_h: usize,
    model: AlignModel,
) -> Option<[[f64; 3]; 3]> {
    let pattern = fixed_test_pairs();
    let ref_f = features::extract_at(ref_rgb, ref_w, ref_h, ref_w, ref_h, &pattern);
    let mov_f = features::extract_at(mov_rgb, mov_w, mov_h, mov_w, mov_h, &pattern);

    // Match moving → reference so the correspondences are already ordered `(p_mov, q_ref)`; the fit
    // then maps mov → ref directly.
    let matches = matching::match_descriptors(&mov_f.descriptors, &ref_f.descriptors);
    if matches.len() < 4 {
        return None;
    }
    let corr: Vec<(Point2<f64>, Point2<f64>)> = matches
        .iter()
        .map(|&(m, r)| {
            let (px, py) = mov_f.points[m];
            let (qx, qy) = ref_f.points[r];
            (Point2::new(px, py), Point2::new(qx, qy))
        })
        .collect();

    // Both frames are full-res here, so their reg_scales are equal; `min` mirrors the pano pipeline.
    let reg_scale = mov_f.reg_scale.min(ref_f.reg_scale);
    // Single pair → a fixed seed offset; RANSAC stays deterministic.
    let pair_index = 0;

    let m = match model {
        AlignModel::Affine => ransac::verify_pair_affine(&corr, reg_scale, pair_index)?,
        AlignModel::Homography => ransac::verify_pair(&corr, reg_scale, pair_index)?.h,
    };
    Some(mat3_to_array(&m))
}

/// Row-major `[[f64;3];3]` from a nalgebra `Matrix3`, keeping nalgebra out of the public signature.
fn mat3_to_array(m: &Matrix3<f64>) -> [[f64; 3]; 3] {
    [
        [m[(0, 0)], m[(0, 1)], m[(0, 2)]],
        [m[(1, 0)], m[(1, 1)], m[(1, 2)]],
        [m[(2, 0)], m[(2, 1)], m[(2, 2)]],
    ]
}
