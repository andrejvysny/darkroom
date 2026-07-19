//! Camera-model initialization: focal length from pairwise homographies (OpenCV
//! `detail::focalsFromHomography` / `estimateFocal`), then rotation seeding by walking the spanning
//! tree (OpenCV `HomographyBasedEstimator` / `CalcRotation`).
//!
//! Ports are faithful to OpenCV 4.x `modules/stitching/src/autocalib.cpp` and
//! `motion_estimators.cpp` — the direction conventions in particular are copied, not guessed.

use nalgebra::Matrix3;

use crate::graph::SpanningTree;

/// One camera's intrinsics + orientation seed.
#[derive(Clone)]
pub(crate) struct CameraInit {
    pub focal: f64,
    pub ppx: f64,
    pub ppy: f64,
    pub rotation: Matrix3<f64>,
}

/// OpenCV `detail::focalsFromHomography`, ported verbatim. `H` is `q ≈ H·p` in pixel coordinates.
/// Returns `(f0, f1)` where each is `Some` iff the corresponding estimate was well-conditioned.
/// `h[]` indexing is row-major to match OpenCV's `H.ptr<double>()`.
pub(crate) fn focals_from_homography(h: &Matrix3<f64>) -> (Option<f64>, Option<f64>) {
    let h0 = h[(0, 0)];
    let h1 = h[(0, 1)];
    let h2 = h[(0, 2)];
    let h3 = h[(1, 0)];
    let h4 = h[(1, 1)];
    let h5 = h[(1, 2)];
    let h6 = h[(2, 0)];
    let h7 = h[(2, 1)];

    // f1 (the "to" image focal in OpenCV's derivation).
    let mut d1 = h6 * h7;
    let mut d2 = (h7 - h6) * (h7 + h6);
    let mut v1 = -(h0 * h1 + h3 * h4) / d1;
    let mut v2 = (h0 * h0 + h3 * h3 - h1 * h1 - h4 * h4) / d2;
    if v1 < v2 {
        std::mem::swap(&mut v1, &mut v2);
        std::mem::swap(&mut d1, &mut d2);
    }
    let f1 = if v1 > 0.0 && v2 > 0.0 {
        Some((if d1.abs() > d2.abs() { v1 } else { v2 }).sqrt())
    } else if v1 > 0.0 {
        Some(v1.sqrt())
    } else {
        None
    };

    // f0 (the "from" image focal).
    let mut d1 = h0 * h3 + h1 * h4;
    let mut d2 = h0 * h0 + h1 * h1 - h3 * h3 - h4 * h4;
    let mut v1 = -h2 * h5 / d1;
    let mut v2 = (h5 * h5 - h2 * h2) / d2;
    if v1 < v2 {
        std::mem::swap(&mut v1, &mut v2);
        std::mem::swap(&mut d1, &mut d2);
    }
    let f0 = if v1 > 0.0 && v2 > 0.0 {
        Some((if d1.abs() > d2.abs() { v1 } else { v2 }).sqrt())
    } else if v1 > 0.0 {
        Some(v1.sqrt())
    } else {
        None
    };

    (f0, f1)
}

/// A single shared focal for all cameras (OpenCV assumes one focal at init). Collects every
/// well-conditioned `f0`/`f1` estimate across the verified homographies and takes the median. Falls
/// back to `focal_seed_px` if provided, else a wide-angle guess of `0.8 · full_width`.
pub(crate) fn estimate_focal(
    homographies: &[Matrix3<f64>],
    full_width: f64,
    focal_seed_px: Option<f32>,
) -> f64 {
    let mut estimates: Vec<f64> = Vec::new();
    for h in homographies {
        let (f0, f1) = focals_from_homography(h);
        for f in [f0, f1].into_iter().flatten() {
            if f.is_finite() && f > 0.0 {
                estimates.push(f);
            }
        }
    }
    if estimates.is_empty() {
        return focal_seed_px.map(|s| s as f64).unwrap_or(0.8 * full_width);
    }
    estimates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = estimates.len();
    if n % 2 == 1 {
        estimates[n / 2]
    } else {
        0.5 * (estimates[n / 2 - 1] + estimates[n / 2])
    }
}

fn intrinsics(focal: f64, ppx: f64, ppy: f64) -> Matrix3<f64> {
    Matrix3::new(focal, 0.0, ppx, 0.0, focal, ppy, 0.0, 0.0, 1.0)
}

/// Nearest rotation matrix to `r` (Kabsch/SVD): `R = U · diag(1,1,det(UVᵀ)) · Vᵀ`, so `det = +1`.
fn orthonormalize(r: Matrix3<f64>) -> Matrix3<f64> {
    let svd = r.svd(true, true);
    let (u, v_t) = (svd.u.unwrap(), svd.v_t.unwrap());
    let d = (u * v_t).determinant();
    let s = Matrix3::new(1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, d.signum());
    u * s * v_t
}

/// Seed each used camera's rotation by walking the spanning tree from the reference (`R = I`).
///
/// Convention note: OpenCV's `CameraParams.R` is **camera→world** (its warper forms world rays as
/// `R·K⁻¹·x`). This crate instead uses **world→camera** rotations throughout (bundle/wave/rendering
/// form world rays as `Rᵀ·K⁻¹·x`), which is the transpose of OpenCV's. So rather than copy
/// `CalcRotation` verbatim we re-derive it for our convention. With `H_{p→c} = K_c·R_c·R_pᵀ·K_p⁻¹`
/// (the pixel map a rotating camera induces), solving for the child rotation gives
/// `R_c = K_c⁻¹ · H_{p→c} · K_p · R_p`.
///
/// `pairwise[pair_idx]` stores `(i, j, H_{i→j})`; if the tree step runs `parent → child` but the pair
/// was stored `child → parent`, we invert to obtain `H_{parent→child}`. Returns a vector
/// index-aligned with all frames (`None` for frames outside the panorama).
pub(crate) fn seed_cameras(
    tree: &SpanningTree,
    pairwise: &[(usize, usize, Matrix3<f64>)],
    focal: f64,
    principal_points: &[(f64, f64)],
) -> Vec<Option<CameraInit>> {
    let n_frames = principal_points.len();
    let mut cams: Vec<Option<CameraInit>> = vec![None; n_frames];

    let mk = |idx: usize, rotation: Matrix3<f64>| {
        let (ppx, ppy) = principal_points[idx];
        CameraInit {
            focal,
            ppx,
            ppy,
            rotation,
        }
    };

    cams[tree.reference] = Some(mk(tree.reference, Matrix3::identity()));

    for step in &tree.order {
        let (pi, pj, h_ij) = &pairwise[step.pair_idx];
        // Resolve H_{parent→child}.
        let h_pc = if *pi == step.parent && *pj == step.child {
            *h_ij
        } else if *pi == step.child && *pj == step.parent {
            h_ij.try_inverse().unwrap_or_else(Matrix3::identity)
        } else {
            // Should never happen: the tree edge's pair_idx must reference exactly this pair.
            unreachable!("tree edge pair mismatch");
        };

        let parent = cams[step.parent]
            .as_ref()
            .expect("parent must be seeded before child");
        let k_parent = intrinsics(parent.focal, parent.ppx, parent.ppy);
        let (cppx, cppy) = principal_points[step.child];
        let k_child = intrinsics(focal, cppx, cppy);

        let k_child_inv = k_child.try_inverse().unwrap_or_else(Matrix3::identity);
        // R_c = K_c⁻¹ · H_{p→c} · K_p · R_p  (world→camera convention, see doc comment).
        let r_child = orthonormalize(k_child_inv * h_pc * k_parent * parent.rotation);

        cams[step.child] = Some(mk(step.child, r_child));
    }

    cams
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rot_y(theta: f64) -> Matrix3<f64> {
        let (s, c) = theta.sin_cos();
        Matrix3::new(c, 0.0, s, 0.0, 1.0, 0.0, -s, 0.0, c)
    }

    fn rot_x(theta: f64) -> Matrix3<f64> {
        let (s, c) = theta.sin_cos();
        Matrix3::new(1.0, 0.0, 0.0, 0.0, c, -s, 0.0, s, c)
    }

    #[test]
    fn focal_from_rotation_homography() {
        // With principal point at the origin (the assumption baked into focalsFromHomography),
        // H = K·R·K⁻¹ for K = diag(f, f, 1) recovers f exactly.
        //
        // NOTE: a rotation about a single *image-aligned* axis (e.g. pure rot_y) zeroes the cross
        // terms h1/h7, making one of OpenCV's two denominators 0 and its estimate 0/0 = NaN — the
        // formula is genuinely degenerate there (OpenCV behaves identically). Real rigs are never
        // perfectly axis-aligned, so we test a compound yaw+pitch rotation, which is well conditioned.
        let f = 1000.0;
        let k = Matrix3::new(f, 0.0, 0.0, 0.0, f, 0.0, 0.0, 0.0, 1.0);
        let k_inv = k.try_inverse().unwrap();
        let r = rot_y(10.0_f64.to_radians()) * rot_x(6.0_f64.to_radians());
        let h = k * r * k_inv;

        let (f0, f1) = focals_from_homography(&h);
        let f0 = f0.expect("f0 estimate should be well-conditioned");
        let f1 = f1.expect("f1 estimate should be well-conditioned");
        assert!((f0 - f).abs() < 0.01 * f, "f0 = {f0}, want ~{f}");
        assert!((f1 - f).abs() < 0.01 * f, "f1 = {f1}, want ~{f}");
    }
}
