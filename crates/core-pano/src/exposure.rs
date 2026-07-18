//! Per-channel gain compensation (Brown & Lowe gain error) at the seam scale.
//!
//! For every unordered pair of used frames whose canvas rects overlap, we accumulate the mean
//! intensity each frame contributes over the mutual-overlap pixels (those where BOTH feather weights
//! clear a small threshold) together with the overlap pixel count. Solving, per colour channel, the
//! regularized least squares
//!
//! ```text
//!   minimise  Σ_pairs N_ij (g_i·Ī_i − g_j·Ī_j)²  +  λ Σ_i N_i (g_i − 1)²
//! ```
//!
//! (λ = 0.01, `N_i` = frame `i`'s total overlap pixel count) yields per-frame gains that equalize the
//! seams while the regularizer keeps the global scale near 1. The system is tiny (cameras ≤ ~10), so
//! it is solved as a dense symmetric positive-definite system via Cholesky. Gains are clamped to
//! `[0.5, 2.0]` and returned index-aligned with `used_indices`; they multiply frame pixels at blend.

use nalgebra::{Cholesky, DMatrix, DVector};

use crate::project::Warped;

/// Feather-weight floor for a pixel to count as covered by a frame in the overlap statistics.
const WEIGHT_FLOOR: f32 = 0.05;
/// Tikhonov weight pulling gains toward 1 (keeps the global exposure gauge fixed).
const LAMBDA: f64 = 0.01;
/// Gain clamp range.
const GAIN_MIN: f32 = 0.5;
const GAIN_MAX: f32 = 2.0;

/// Accumulated overlap statistics for one unordered pair.
struct PairStat {
    i: usize,
    j: usize,
    /// Mean intensity of frame `i` / `j` over the overlap, per channel.
    mean_i: [f64; 3],
    mean_j: [f64; 3],
    /// Overlap pixel count.
    n: f64,
}

/// Compute per-frame, per-channel gains from the low-resolution warps (index-aligned with the warp
/// slice / `used_indices`). Slots whose warp is `None` get unit gain.
pub(crate) fn solve_gains(warps: &[Option<Warped>]) -> Vec<[f32; 3]> {
    let n_cam = warps.len();
    let mut gains = vec![[1.0f32; 3]; n_cam];
    if n_cam == 0 {
        return gains;
    }

    // --- Accumulate overlap statistics over every used pair with rect overlap. ---
    let mut stats: Vec<PairStat> = Vec::new();
    for i in 0..n_cam {
        for j in (i + 1)..n_cam {
            let (Some(wi), Some(wj)) = (&warps[i], &warps[j]) else {
                continue;
            };
            let Some(rect) = rect_overlap(wi.rect, wj.rect) else {
                continue;
            };
            let (x0, y0, ow, oh) = rect;
            let mut sum_i = [0.0f64; 3];
            let mut sum_j = [0.0f64; 3];
            let mut count = 0.0f64;
            for cy in y0..y0 + oh {
                for cx in x0..x0 + ow {
                    let (Some((ci, wgi)), Some((cj, wgj))) = (wi.sample(cx, cy), wj.sample(cx, cy))
                    else {
                        continue;
                    };
                    if wgi <= WEIGHT_FLOOR || wgj <= WEIGHT_FLOOR {
                        continue;
                    }
                    for c in 0..3 {
                        sum_i[c] += ci[c] as f64;
                        sum_j[c] += cj[c] as f64;
                    }
                    count += 1.0;
                }
            }
            if count < 1.0 {
                continue;
            }
            stats.push(PairStat {
                i,
                j,
                mean_i: [sum_i[0] / count, sum_i[1] / count, sum_i[2] / count],
                mean_j: [sum_j[0] / count, sum_j[1] / count, sum_j[2] / count],
                n: count,
            });
        }
    }
    if stats.is_empty() {
        return gains;
    }

    // Total overlap pixel count per frame (the regularizer weight N_i).
    let mut n_i = vec![0.0f64; n_cam];
    for s in &stats {
        n_i[s.i] += s.n;
        n_i[s.j] += s.n;
    }

    // --- Solve one dense SPD system per channel. ---
    for c in 0..3 {
        // Normalize intensities to ~O(1) before building the system. Brown & Lowe's λ = 0.01 assumes
        // O(1)-scale intensities (they work on ~0..255 data); camera-native linear values are dim
        // (~0.05..0.2), which would let the regularizer swamp the weak data term and leave the gains
        // stuck near 1. Scaling every mean by `norm` (its reciprocal average magnitude) restores the
        // intended balance and leaves the recovered gain *ratios* unchanged.
        let mut scale_sum = 0.0f64;
        let mut scale_n = 0.0f64;
        for s in &stats {
            scale_sum += s.mean_i[c].abs() + s.mean_j[c].abs();
            scale_n += 2.0;
        }
        let norm = if scale_sum > 1e-12 {
            scale_n / scale_sum
        } else {
            1.0
        };

        let mut a = DMatrix::<f64>::zeros(n_cam, n_cam);
        let mut b = DVector::<f64>::zeros(n_cam);
        for s in &stats {
            let (ii, jj) = (s.i, s.j);
            let (ai, aj) = (s.mean_i[c] * norm, s.mean_j[c] * norm);
            a[(ii, ii)] += s.n * ai * ai;
            a[(jj, jj)] += s.n * aj * aj;
            a[(ii, jj)] -= s.n * ai * aj;
            a[(jj, ii)] -= s.n * ai * aj;
        }
        for k in 0..n_cam {
            a[(k, k)] += LAMBDA * n_i[k];
            b[k] += LAMBDA * n_i[k];
        }
        if let Some(chol) = Cholesky::new(a) {
            let g = chol.solve(&b);
            for (k, gain) in gains.iter_mut().enumerate() {
                gain[c] = (g[k] as f32).clamp(GAIN_MIN, GAIN_MAX);
            }
        }
    }

    gains
}

/// Intersection of two `(x0,y0,w,h)` canvas rects, or `None` if disjoint.
fn rect_overlap(
    a: (usize, usize, usize, usize),
    b: (usize, usize, usize, usize),
) -> Option<(usize, usize, usize, usize)> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    if x1 > x0 && y1 > y0 {
        Some((x0, y0, x1 - x0, y1 - y0))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat-intensity warp over `rect` with all-1 feather weight and constant colour `v`.
    fn flat_warp(rect: (usize, usize, usize, usize), v: f32) -> Warped {
        let (_, _, w, h) = rect;
        Warped {
            rect,
            rgb: vec![v; w * h * 3],
            weight: vec![1.0; w * h],
        }
    }

    #[test]
    fn recovers_compensating_gains() {
        // Three horizontally-overlapping frames. Frame 0 is darkened ×0.7, frame 2 brightened ×1.4
        // (base scene intensity 0.5). The solver should recover gains that cancel those offsets:
        // g0/g1 ≈ 1/0.7, g2/g1 ≈ 1/1.4.
        let base = 0.5f32;
        let warps = vec![
            Some(flat_warp((0, 0, 200, 100), base * 0.7)),
            Some(flat_warp((100, 0, 200, 100), base)),
            Some(flat_warp((200, 0, 200, 100), base * 1.4)),
        ];
        let gains = solve_gains(&warps);
        let (g0, g1, g2) = (gains[0][0], gains[1][0], gains[2][0]);
        let r01 = g0 / g1;
        let r21 = g2 / g1;
        assert!(
            (r01 - 1.0 / 0.7).abs() < 0.10 * (1.0 / 0.7),
            "g0/g1 = {r01}, want ~{}",
            1.0 / 0.7
        );
        assert!(
            (r21 - 1.0 / 1.4).abs() < 0.10 * (1.0 / 1.4),
            "g2/g1 = {r21}, want ~{}",
            1.0 / 1.4
        );
        // All three channels are driven identically, so per-channel gains must agree.
        for g in &gains {
            assert!((g[0] - g[1]).abs() < 1e-4 && g[1] - g[2] < 1e-4);
        }
    }
}
