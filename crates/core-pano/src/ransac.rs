//! Robust pairwise homography estimation: normalized 4-point DLT inside adaptive RANSAC, followed by
//! Brown & Lowe probabilistic match verification.
//!
//! All geometry runs in FULL-RES pixel coordinates (keypoints are stored full-res so the focal/
//! rotation math downstream is in the camera's real pixel units). The inlier threshold is specified
//! as 3 px at *registration scale*; since points are full-res we scale it up by `1 / reg_scale`.
//!
//! Determinism: sampling uses `SplitMix64(0xC0FFEE + pair_index)` — no global RNG — so a given match
//! set always yields the same homography.

use nalgebra::{Matrix3, Point2, SMatrix, Vector3};

use crate::rng::SplitMix64;

/// Symmetric-transfer inlier threshold, expressed in registration-scale pixels.
const INLIER_THRESH_REG_PX: f64 = 3.0;
/// Maximum RANSAC iterations (adaptive early-exit usually stops far sooner).
const MAX_ITERS: usize = 2000;
/// Target confidence for the adaptive iteration count (standard `1-(1-w^4)^N ≥ 0.99`).
const CONFIDENCE: f64 = 0.99;

/// A pair that passed RANSAC + Brown&Lowe verification.
pub(crate) struct VerifiedPair {
    /// Homography mapping image-i points to image-j points (`q ≈ H·p`), full-res pixels.
    pub h: Matrix3<f64>,
    pub inliers: Vec<(Point2<f64>, Point2<f64>)>,
    pub n_inliers: usize,
}

/// Run RANSAC then the Brown&Lowe gate on candidate correspondences `(p_i, q_j)`.
/// `reg_scale` converts the registration-scale threshold to full-res; `pair_index` seeds sampling.
/// Returns `None` if the pair is not a confident overlap.
pub(crate) fn verify_pair(
    corr: &[(Point2<f64>, Point2<f64>)],
    reg_scale: f64,
    pair_index: u64,
) -> Option<VerifiedPair> {
    let res = ransac_homography(corr, reg_scale, pair_index)?;
    let n_inliers = res.inliers.len();
    let n_matches = corr.len();
    // Brown & Lowe (2007): a pair is a true overlap iff the inlier count clears both a fixed floor
    // and a fraction of the candidate matches — `n_i > 8 + 0.3·n_f` and `n_i ≥ 15`.
    if n_inliers >= 15 && (n_inliers as f64) > 8.0 + 0.3 * n_matches as f64 {
        Some(VerifiedPair {
            h: res.h,
            inliers: res.inliers,
            n_inliers,
        })
    } else {
        None
    }
}

struct RansacResult {
    h: Matrix3<f64>,
    inliers: Vec<(Point2<f64>, Point2<f64>)>,
}

fn ransac_homography(
    corr: &[(Point2<f64>, Point2<f64>)],
    reg_scale: f64,
    pair_index: u64,
) -> Option<RansacResult> {
    let n = corr.len();
    if n < 4 {
        return None;
    }
    let thresh = INLIER_THRESH_REG_PX / reg_scale;
    let thresh_sq = thresh * thresh;

    let mut rng = SplitMix64::new(0xC0FFEE_u64.wrapping_add(pair_index));
    let mut best_inliers: Vec<usize> = Vec::new();
    let mut dynamic_iters = MAX_ITERS;

    let mut iter = 0usize;
    while iter < MAX_ITERS && iter < dynamic_iters {
        iter += 1;
        let sample = pick4(&mut rng, n);
        let subset: [(Point2<f64>, Point2<f64>); 4] = [
            corr[sample[0]],
            corr[sample[1]],
            corr[sample[2]],
            corr[sample[3]],
        ];
        let Some(h) = dlt(&subset) else { continue };
        let Some(hinv) = h.try_inverse() else {
            continue;
        };

        let inliers: Vec<usize> = (0..n)
            .filter(|&k| transfer_err_sq(&h, &hinv, &corr[k].0, &corr[k].1) < thresh_sq)
            .collect();

        if inliers.len() > best_inliers.len() {
            best_inliers = inliers;
            // Adaptive early exit: shrink the required iteration count as the best inlier ratio grows.
            let w = best_inliers.len() as f64 / n as f64;
            let w4 = w * w * w * w;
            if w4 > 0.0 && w4 < 1.0 {
                let need = (1.0 - CONFIDENCE).ln() / (1.0 - w4).ln();
                dynamic_iters = (need.ceil() as usize).clamp(1, MAX_ITERS);
            } else if w4 >= 1.0 {
                dynamic_iters = iter; // perfect consensus, stop
            }
        }
    }

    if best_inliers.len() < 4 {
        return None;
    }

    // Refit over ALL inliers (DLT is a least-squares fit; the 4-point model was only for consensus).
    let inlier_pts: Vec<(Point2<f64>, Point2<f64>)> =
        best_inliers.iter().map(|&k| corr[k]).collect();
    let h = dlt(&inlier_pts)?;
    let hinv = h.try_inverse()?;
    let inliers: Vec<(Point2<f64>, Point2<f64>)> = corr
        .iter()
        .copied()
        .filter(|(p, q)| transfer_err_sq(&h, &hinv, p, q) < thresh_sq)
        .collect();

    Some(RansacResult { h, inliers })
}

/// Squared symmetric transfer error: `‖q − H·p‖² + ‖p − H⁻¹·q‖²` (both directions, in target pixels).
fn transfer_err_sq(h: &Matrix3<f64>, hinv: &Matrix3<f64>, p: &Point2<f64>, q: &Point2<f64>) -> f64 {
    let fwd = apply(h, p);
    let bwd = apply(hinv, q);
    let (dx, dy) = (fwd.x - q.x, fwd.y - q.y);
    let (ex, ey) = (bwd.x - p.x, bwd.y - p.y);
    dx * dx + dy * dy + ex * ex + ey * ey
}

/// Apply a homography to a point; returns a huge sentinel distance component if the point maps to the
/// line at infinity (near-zero homogeneous w), which naturally excludes it as an outlier.
#[inline]
fn apply(h: &Matrix3<f64>, p: &Point2<f64>) -> Point2<f64> {
    let v = h * Vector3::new(p.x, p.y, 1.0);
    if v.z.abs() < 1e-12 {
        Point2::new(1e18, 1e18)
    } else {
        Point2::new(v.x / v.z, v.y / v.z)
    }
}

/// Pick 4 distinct indices in `[0, n)` deterministically.
fn pick4(rng: &mut SplitMix64, n: usize) -> [usize; 4] {
    let mut out = [0usize; 4];
    let mut count = 0;
    while count < 4 {
        let cand = rng.gen_range(n);
        if !out[..count].contains(&cand) {
            out[count] = cand;
            count += 1;
        }
    }
    out
}

/// Normalized Direct Linear Transform. Hartley-normalizes both point sets (centroid at origin, RMS
/// distance √2), builds the homogeneous system `A·h = 0`, and solves it as the smallest-eigenvalue
/// eigenvector of `AᵀA` (works for the minimal 4-point case and the over-determined refit alike; a
/// thin SVD of the 8×9 minimal system would omit the null vector, so we use `AᵀA` instead).
fn dlt(corr: &[(Point2<f64>, Point2<f64>)]) -> Option<Matrix3<f64>> {
    let (t_src, src_n) = hartley_normalize(corr.iter().map(|c| c.0))?;
    let (t_dst, dst_n) = hartley_normalize(corr.iter().map(|c| c.1))?;

    // Accumulate M = AᵀA from the two rows contributed by each correspondence.
    let mut m = SMatrix::<f64, 9, 9>::zeros();
    for (p, q) in src_n.iter().zip(dst_n.iter()) {
        let (x, y) = (p.x, p.y);
        let (xp, yp) = (q.x, q.y);
        let r1 = [-x, -y, -1.0, 0.0, 0.0, 0.0, xp * x, xp * y, xp];
        let r2 = [0.0, 0.0, 0.0, -x, -y, -1.0, yp * x, yp * y, yp];
        for r in [&r1, &r2] {
            for a in 0..9 {
                for b in 0..9 {
                    m[(a, b)] += r[a] * r[b];
                }
            }
        }
    }

    // Smallest-eigenvalue eigenvector of the symmetric PSD matrix M.
    let eig = m.symmetric_eigen();
    let mut min_idx = 0;
    let mut min_val = eig.eigenvalues[0];
    for i in 1..9 {
        if eig.eigenvalues[i] < min_val {
            min_val = eig.eigenvalues[i];
            min_idx = i;
        }
    }
    let h = eig.eigenvectors.column(min_idx);

    let h_norm = Matrix3::new(h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], h[8]);

    // Denormalize: H = T_dst⁻¹ · H_norm · T_src.
    let t_dst_inv = t_dst.try_inverse()?;
    let mut hmat = t_dst_inv * h_norm * t_src;

    // Scale-normalize for numerical tidiness (focal/rotation use is scale-invariant either way).
    let s = hmat[(2, 2)];
    if s.abs() > 1e-12 {
        hmat /= s;
    } else {
        let f = hmat.norm();
        if f < 1e-12 {
            return None;
        }
        hmat /= f;
    }
    Some(hmat)
}

/// Hartley normalization: returns the similarity transform `T` and the transformed points. `T` maps
/// the input points to zero mean and RMS distance √2 from the origin.
fn hartley_normalize<I: Iterator<Item = Point2<f64>>>(
    pts: I,
) -> Option<(Matrix3<f64>, Vec<Point2<f64>>)> {
    let pts: Vec<Point2<f64>> = pts.collect();
    let n = pts.len();
    if n == 0 {
        return None;
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for p in &pts {
        cx += p.x;
        cy += p.y;
    }
    cx /= n as f64;
    cy /= n as f64;

    let mut mean_dist = 0.0;
    for p in &pts {
        mean_dist += ((p.x - cx).powi(2) + (p.y - cy).powi(2)).sqrt();
    }
    mean_dist /= n as f64;
    if mean_dist < 1e-12 {
        return None; // all points coincident — degenerate
    }
    let scale = std::f64::consts::SQRT_2 / mean_dist;

    let t = Matrix3::new(
        scale,
        0.0,
        -scale * cx,
        0.0,
        scale,
        -scale * cy,
        0.0,
        0.0,
        1.0,
    );
    let normed = pts
        .iter()
        .map(|p| Point2::new(scale * (p.x - cx), scale * (p.y - cy)))
        .collect();
    Some((t, normed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::SplitMix64;

    /// A known mildly-projective homography (≈5° rotation + translation + small perspective).
    fn known_h() -> Matrix3<f64> {
        Matrix3::new(
            0.995, -0.087, 12.0, 0.087, 0.995, -6.0, 1.0e-4, -5.0e-5, 1.0,
        )
    }

    #[test]
    fn recovers_homography_with_thirty_percent_outliers() {
        let h_true = known_h();
        let mut rng = SplitMix64::new(0x1234_5678);
        let mut corr: Vec<(Point2<f64>, Point2<f64>)> = Vec::new();

        // 200 exact inliers.
        const N_IN: usize = 200;
        for _ in 0..N_IN {
            let x = 50.0 + rng.next_f64() * 540.0;
            let y = 50.0 + rng.next_f64() * 380.0;
            let src = Point2::new(x, y);
            let dst = apply(&h_true, &src);
            corr.push((src, dst));
        }
        // 86 gross outliers (~30% of the 286 total): random, unrelated destinations.
        const N_OUT: usize = 86;
        for _ in 0..N_OUT {
            let src = Point2::new(50.0 + rng.next_f64() * 540.0, 50.0 + rng.next_f64() * 380.0);
            let dst = Point2::new(rng.next_f64() * 640.0, rng.next_f64() * 480.0);
            corr.push((src, dst));
        }

        let res = ransac_homography(&corr, 1.0, 0).expect("RANSAC should find a model");

        // Recovered H must map test points essentially identically to the ground-truth H.
        for &(tx, ty) in &[
            (100.0, 100.0),
            (500.0, 120.0),
            (300.0, 400.0),
            (600.0, 450.0),
        ] {
            let p = Point2::new(tx, ty);
            let a = apply(&res.h, &p);
            let b = apply(&h_true, &p);
            assert!(
                (a.x - b.x).abs() < 1e-2 && (a.y - b.y).abs() < 1e-2,
                "point ({tx},{ty}) mapped to {a:?} vs truth {b:?}"
            );
        }
        // Should recover (nearly) all 200 true inliers.
        assert!(
            res.inliers.len() >= N_IN - 2,
            "expected >= {} inliers, got {}",
            N_IN - 2,
            res.inliers.len()
        );
    }
}
