//! The optimizer: class-balanced logistic regression with a within-burst ranking term.
//!
//! Pointwise BCE alone answers "is this image a keeper in general"; the pairwise (Bradley-Terry) term
//! answers "is this the keeper *of this burst*", which is the question the UI actually asks. Both are
//! normalized by their own weight mass so `pair_alpha` is a meaningful mix knob rather than a function
//! of how many bursts happen to be in the training set.
//!
//! Numerics follow the presence probe (`core-analyze/examples/train_presence.rs`): standardize
//! per-column for conditioning, run batch gradient descent, then FOLD the standardization back into
//! the weights so scoring at runtime is a plain `sigmoid(w·x + b)` — no mean/std needed at inference.
//! Sums use `f64` accumulators; everything stored is `f32`.

use rayon::prelude::*;

use crate::sample::{Pair, Sample};
use crate::weights::compute_weights;

/// Optimizer settings. Defaults match the presence probe's proven schedule.
#[derive(Debug, Clone)]
pub struct TrainConfig {
    pub iters: usize,
    pub lr: f32,
    pub lambda: f32,
    /// Mix weight of the pairwise ranking loss relative to pointwise BCE.
    pub pair_alpha: f32,
    /// Previous model as `[folded w…, b]` (raw space, length `d + 1`). Un-folded into standardized
    /// space to seed GD, so a retrain nudges the existing model instead of restarting from zero.
    /// Ignored if the length does not match.
    pub warm_start: Option<Vec<f32>>,
}

impl Default for TrainConfig {
    fn default() -> Self {
        Self {
            iters: 1500,
            lr: 0.5,
            lambda: 1e-2,
            pair_alpha: 1.0,
            warm_start: None,
        }
    }
}

/// Fitted head in **raw** feature space, plus the imputation means it was fit against (scoring must
/// reuse them — a different fill would move every NaN row off the surface the weights were fit to).
#[derive(Debug, Clone)]
pub struct FitResult {
    pub w: Vec<f32>,
    pub b: f32,
    pub impute_means: Vec<f32>,
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

/// `ln(1 + e^x)`, branch-stabilized against overflow for large |x|.
fn softplus(x: f64) -> f64 {
    if x > 0.0 {
        x + (-x).exp().ln_1p()
    } else {
        x.exp().ln_1p()
    }
}

fn dot(w: &[f32], z: &[f32]) -> f64 {
    w.iter().zip(z).map(|(a, b)| *a as f64 * *b as f64).sum()
}

/// Weighted per-column mean over the finite values only. Columns with no weighted finite value fall
/// back to the unweighted finite mean, then to 0.0 (a column that is NaN everywhere carries no
/// signal; it standardizes to a constant and the fit ignores it).
pub(crate) fn weighted_column_means(rows: &[Vec<f32>], w: &[f32], d: usize) -> Vec<f32> {
    let mut num = vec![0f64; d];
    let mut den = vec![0f64; d];
    for (row, &wi) in rows.iter().zip(w) {
        if wi <= 0.0 {
            continue;
        }
        for (j, &v) in row.iter().enumerate().take(d) {
            if v.is_finite() {
                num[j] += wi as f64 * v as f64;
                den[j] += wi as f64;
            }
        }
    }
    (0..d)
        .map(|j| {
            if den[j] > 0.0 {
                return (num[j] / den[j]) as f32;
            }
            let finite: Vec<f64> = rows
                .iter()
                .filter(|r| r[j].is_finite())
                .map(|r| r[j] as f64)
                .collect();
            if finite.is_empty() {
                0.0
            } else {
                (finite.iter().sum::<f64>() / finite.len() as f64) as f32
            }
        })
        .collect()
}

pub(crate) fn impute_in_place(x: &mut [f32], means: &[f32]) {
    for (v, &m) in x.iter_mut().zip(means) {
        if !v.is_finite() {
            *v = m;
        }
    }
}

/// `sigmoid(w·x + b)` with NaNs in `x` replaced by `means` on the fly (no allocation).
pub(crate) fn score_with(w: &[f32], b: f32, means: &[f32], x: &[f32]) -> f32 {
    let z: f64 = w
        .iter()
        .zip(x)
        .zip(means)
        .map(|((&wj, &xj), &mj)| wj as f64 * if xj.is_finite() { xj as f64 } else { mj as f64 })
        .sum();
    sigmoid(b as f64 + z) as f32
}

/// Per-column `(mean, std)` over the rows that actually train (weight > 0); std floored so a constant
/// column cannot divide by zero. Zero-weight rows (`Batch` labels) are excluded — they are logged
/// history, not training distribution, and must not move the scaling the L2 penalty is measured in.
fn standardizer(rows: &[Vec<f32>], w: &[f32], d: usize) -> (Vec<f32>, Vec<f32>) {
    let rows: Vec<&Vec<f32>> = rows
        .iter()
        .zip(w)
        .filter(|(_, &wi)| wi > 0.0)
        .map(|(r, _)| r)
        .collect();
    let n = rows.len().max(1) as f64;
    let mut mean = vec![0f64; d];
    for row in &rows {
        for (m, &v) in mean.iter_mut().zip(row.iter()) {
            *m += v as f64;
        }
    }
    mean.iter_mut().for_each(|m| *m /= n);
    let mut var = vec![0f64; d];
    for row in &rows {
        for ((s, &v), &m) in var.iter_mut().zip(row.iter()).zip(&mean) {
            let dz = v as f64 - m;
            *s += dz * dz;
        }
    }
    (
        mean.iter().map(|&m| m as f32).collect(),
        var.iter()
            .map(|&s| ((s / n).sqrt() as f32).max(1e-6))
            .collect(),
    )
}

fn standardize(row: &[f32], mean: &[f32], std: &[f32]) -> Vec<f32> {
    row.iter()
        .zip(mean)
        .zip(std)
        .map(|((&v, &m), &s)| (v - m) / s)
        .collect()
}

/// Fold standardization back into the weights: `sigmoid(w·z + b) == sigmoid(wf·x + bf)`.
fn fold(w: &[f32], b: f32, mean: &[f32], std: &[f32]) -> (Vec<f32>, f32) {
    let wf: Vec<f32> = w.iter().zip(std).map(|(&wj, &s)| wj / s).collect();
    let shift: f64 = wf
        .iter()
        .zip(mean)
        .map(|(&wj, &m)| wj as f64 * m as f64)
        .sum();
    (wf, (b as f64 - shift) as f32)
}

/// Inverse of [`fold`] — seeds GD in standardized space from a raw-space warm start.
fn unfold(raw: &[f32], mean: &[f32], std: &[f32]) -> (Vec<f32>, f32) {
    let d = mean.len();
    let (wf, bf) = (&raw[..d], raw[d]);
    let w: Vec<f32> = wf.iter().zip(std).map(|(&wj, &s)| wj * s).collect();
    let shift: f64 = wf
        .iter()
        .zip(mean)
        .map(|(&wj, &m)| wj as f64 * m as f64)
        .sum();
    (w, (bf as f64 + shift) as f32)
}

/// The objective, evaluated in standardized space. `sw` is aligned to `z`; `pairs` index into `z`.
struct Problem<'a> {
    z: &'a [Vec<f32>],
    y: &'a [bool],
    sw: &'a [f32],
    sw_sum: f64,
    pairs: &'a [Pair],
    pw_sum: f64,
    lambda: f64,
    pair_alpha: f64,
}

type Grad = (f64, Vec<f64>, f64);

impl Problem<'_> {
    /// Σ wᵢ·BCE(σ(w·zᵢ+b), yᵢ) and its gradient (un-normalized).
    fn pointwise(&self, w: &[f32], b: f32) -> Grad {
        let d = w.len();
        let init = || (0f64, vec![0f64; d], 0f64);
        self.z
            .par_iter()
            .zip(self.y.par_iter())
            .zip(self.sw.par_iter())
            .filter(|(_, &wi)| wi > 0.0)
            .fold(init, |(mut l, mut g, mut gb), ((zi, &yi), &wi)| {
                let s = b as f64 + dot(w, zi);
                let e = wi as f64 * (sigmoid(s) - if yi { 1.0 } else { 0.0 });
                l += wi as f64 * if yi { softplus(-s) } else { softplus(s) };
                for (gj, &zj) in g.iter_mut().zip(zi) {
                    *gj += e * zj as f64;
                }
                gb += e;
                (l, g, gb)
            })
            .reduce(init, |mut a, b| {
                a.0 += b.0;
                for (x, y) in a.1.iter_mut().zip(&b.1) {
                    *x += y;
                }
                a.2 += b.2;
                a
            })
    }

    /// Σ wₚ·(−log σ(Δₚ)) and its gradient, Δₚ = w·z_win − w·z_lose. The bias cancels in Δ, so the
    /// ranking term never moves `b`.
    fn pairwise(&self, w: &[f32]) -> (f64, Vec<f64>) {
        let d = w.len();
        let init = || (0f64, vec![0f64; d]);
        self.pairs
            .par_iter()
            .fold(init, |(mut l, mut g), p| {
                let (zw, zl) = (&self.z[p.winner], &self.z[p.loser]);
                let delta = dot(w, zw) - dot(w, zl);
                l += p.weight as f64 * softplus(-delta);
                let c = p.weight as f64 * (sigmoid(delta) - 1.0);
                for ((gj, &a), &b) in g.iter_mut().zip(zw).zip(zl) {
                    *gj += c * (a - b) as f64;
                }
                (l, g)
            })
            .reduce(init, |mut a, b| {
                a.0 += b.0;
                for (x, y) in a.1.iter_mut().zip(&b.1) {
                    *x += y;
                }
                a
            })
    }

    /// Full objective + gradient: normalized pointwise + α·normalized pairwise + (λ/2)‖w‖².
    fn loss_grad(&self, w: &[f32], b: f32) -> Grad {
        let (pl, mut gw, gb) = self.pointwise(w, b);
        let denom = self.sw_sum.max(1e-12);
        let mut loss = pl / denom;
        let gb = gb / denom;
        gw.iter_mut().for_each(|g| *g /= denom);

        if !self.pairs.is_empty() && self.pw_sum > 0.0 {
            let (rl, rg) = self.pairwise(w);
            loss += self.pair_alpha * rl / self.pw_sum;
            for (g, r) in gw.iter_mut().zip(&rg) {
                *g += self.pair_alpha * r / self.pw_sum;
            }
        }
        let l2: f64 = w.iter().map(|&x| x as f64 * x as f64).sum();
        loss += 0.5 * self.lambda * l2;
        for (g, &wj) in gw.iter_mut().zip(w) {
            *g += self.lambda * wj as f64;
        }
        (loss, gw, gb)
    }
}

/// Fit the head. `pairs` must index into `samples`; every `x` must have the same length (guaranteed
/// by [`crate::train`], which validates before calling). Ragged or empty input yields a zero model
/// rather than a panic.
pub fn fit(samples: &[Sample], pairs: &[Pair], cfg: &TrainConfig) -> FitResult {
    let d = samples.first().map(|s| s.x.len()).unwrap_or(0);
    if d == 0 || samples.iter().any(|s| s.x.len() != d) {
        return FitResult {
            w: vec![0.0; d],
            b: 0.0,
            impute_means: vec![0.0; d],
        };
    }
    let sw = compute_weights(samples);
    let mut rows: Vec<Vec<f32>> = samples.iter().map(|s| s.x.clone()).collect();
    let impute_means = weighted_column_means(&rows, &sw, d);
    for r in rows.iter_mut() {
        impute_in_place(r, &impute_means);
    }
    let (mean, std) = standardizer(&rows, &sw, d);
    let z: Vec<Vec<f32>> = rows.iter().map(|r| standardize(r, &mean, &std)).collect();
    let y: Vec<bool> = samples.iter().map(|s| s.y).collect();

    let prob = Problem {
        z: &z,
        y: &y,
        sw: &sw,
        sw_sum: sw.iter().map(|&v| v as f64).sum(),
        pairs,
        pw_sum: pairs.iter().map(|p| p.weight as f64).sum(),
        lambda: cfg.lambda as f64,
        pair_alpha: cfg.pair_alpha as f64,
    };
    let (mut w, mut b) = match &cfg.warm_start {
        Some(v) if v.len() == d + 1 => unfold(v, &mean, &std),
        _ => (vec![0.0; d], 0.0),
    };
    let lr = cfg.lr as f64;
    for _ in 0..cfg.iters {
        let (_, gw, gb) = prob.loss_grad(&w, b);
        for (wj, g) in w.iter_mut().zip(&gw) {
            *wj = (*wj as f64 - lr * g) as f32;
        }
        b = (b as f64 - lr * gb) as f32;
    }
    let (w, b) = fold(&w, b, &mean, &std);
    FitResult { w, b, impute_means }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::LabelProvenance;
    use crate::testutil::{sample_x, Lcg};

    /// Central-difference check of [`Problem::loss_grad`] — the one place a sign or normalization slip
    /// would silently produce a plausible-but-wrong model.
    #[test]
    fn gradient_matches_finite_differences() {
        let (n, d) = (30usize, 6usize);
        let mut rng = Lcg::new(0xC0FFEE);
        let z: Vec<Vec<f32>> = (0..n)
            .map(|_| (0..d).map(|_| rng.sym()).collect())
            .collect();
        let y: Vec<bool> = (0..n).map(|i| i % 3 == 0).collect();
        let sw: Vec<f32> = (0..n).map(|_| 0.5 + rng.unit()).collect();
        let pairs: Vec<Pair> = (0..10)
            .map(|i| Pair {
                winner: i * 3,
                loser: i * 3 + 1,
                weight: 0.3 + rng.unit(),
            })
            .collect();
        let prob = Problem {
            z: &z,
            y: &y,
            sw: &sw,
            sw_sum: sw.iter().map(|&v| v as f64).sum(),
            pairs: &pairs,
            pw_sum: pairs.iter().map(|p| p.weight as f64).sum(),
            lambda: 0.02,
            pair_alpha: 0.7,
        };

        let w: Vec<f32> = (0..d).map(|_| rng.sym() * 0.6).collect();
        let b = 0.21f32;
        let (_, gw, gb) = prob.loss_grad(&w, b);
        let h = 1e-3f32;
        for j in 0..d {
            let (mut lo, mut hi) = (w.clone(), w.clone());
            lo[j] -= h;
            hi[j] += h;
            let num =
                (prob.loss_grad(&hi, b).0 - prob.loss_grad(&lo, b).0) / (hi[j] - lo[j]) as f64;
            let rel = (num - gw[j]).abs() / num.abs().max(gw[j].abs()).max(1e-6);
            assert!(rel < 1e-3, "dim {j}: analytic {} vs numeric {num}", gw[j]);
        }
        let num_b = (prob.loss_grad(&w, b + h).0 - prob.loss_grad(&w, b - h).0)
            / ((b + h) - (b - h)) as f64;
        let rel = (num_b - gb).abs() / num_b.abs().max(gb.abs()).max(1e-6);
        assert!(rel < 1e-3, "bias: analytic {gb} vs numeric {num_b}");
    }

    /// With every label in the same class the pointwise term carries no ranking signal, so any
    /// ordering the model learns has to come from the pairwise term.
    #[test]
    fn pairwise_term_alone_learns_the_ranking_direction() {
        let mut rng = Lcg::new(7);
        let d = 5;
        let mut samples = Vec::new();
        let mut pairs = Vec::new();
        for g in 0..20u64 {
            let mut mk = |signal: f32| {
                let mut x: Vec<f32> = (0..d).map(|_| rng.sym() * 0.2).collect();
                x[0] = signal + rng.sym() * 0.1;
                sample_x(x, true, LabelProvenance::Unprompted, g)
            };
            let winner = mk(1.0);
            let loser = mk(-1.0);
            samples.push(winner);
            samples.push(loser);
            pairs.push(Pair {
                winner: samples.len() - 2,
                loser: samples.len() - 1,
                weight: 1.0,
            });
        }
        let cfg = TrainConfig {
            iters: 600,
            lambda: 1e-3,
            ..Default::default()
        };
        let fitres = fit(&samples, &pairs, &cfg);
        assert!(
            fitres.w[0] > 0.0,
            "signal dim should point at the winners: {:?}",
            fitres.w
        );
        for p in &pairs {
            let (sw, sl) = (
                score_with(
                    &fitres.w,
                    fitres.b,
                    &fitres.impute_means,
                    &samples[p.winner].x,
                ),
                score_with(
                    &fitres.w,
                    fitres.b,
                    &fitres.impute_means,
                    &samples[p.loser].x,
                ),
            );
            assert!(sw > sl, "winner {sw} should outrank loser {sl}");
        }
    }

    #[test]
    fn warm_start_round_trips_through_standardization() {
        let (mean, std) = (vec![1.5f32, -2.0], vec![0.5f32, 4.0]);
        let raw = vec![0.3f32, -0.7, 0.11];
        let (w, b) = unfold(&raw, &mean, &std);
        let (wf, bf) = fold(&w, b, &mean, &std);
        assert!((wf[0] - raw[0]).abs() < 1e-6 && (wf[1] - raw[1]).abs() < 1e-6);
        assert!((bf - raw[2]).abs() < 1e-6);
    }

    #[test]
    fn ragged_or_empty_input_yields_a_zero_model() {
        assert!(fit(&[], &[], &TrainConfig::default()).w.is_empty());
        let s = vec![
            sample_x(vec![1.0, 2.0], true, LabelProvenance::Unprompted, 0),
            sample_x(vec![1.0], false, LabelProvenance::Unprompted, 0),
        ];
        assert_eq!(fit(&s, &[], &TrainConfig::default()).w, vec![0.0, 0.0]);
    }
}
