//! Group-aware cross-validation and λ selection.
//!
//! A burst is a set of near-duplicate frames: if one frame trains and its sibling tests, the reported
//! score measures memorization, not generalization. So folds are assigned by **group**, never by
//! sample, and pairs are re-derived inside each training split so no held-out image leaks in through
//! the ranking term either.
//!
//! Metrics are computed only over labels the model could not have influenced
//! ([`LabelProvenance::eval_ok`]) — scoring on agreements would report the model's own echo.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::fit::{fit, score_with, TrainConfig};
use crate::metrics::{auprc, max_f1, precision_threshold, roc_auc, top1_agreement};
use crate::sample::{LabelProvenance, Sample};
use crate::weights::derive_pairs;

/// λ grid searched by [`crate::train`] when the caller passes none.
pub const DEFAULT_LAMBDAS: [f32; 5] = [1e-3, 3e-3, 1e-2, 3e-2, 1e-1];

/// Minimum precision demanded of a "reject this one" suggestion. Deleting/flagging a keeper is far
/// worse than missing a reject, so the reject side gets a precision floor instead of max-F1.
pub const REJECT_MIN_PRECISION: f32 = 0.95;

/// Threshold sentinel meaning "no operating point met the precision floor". Above the range of any
/// probability, so a comparison against it can never fire.
pub const TAU_UNREACHABLE: f32 = 2.0;

/// Metrics for one λ, all from pooled out-of-fold predictions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaResult {
    pub lambda: f32,
    pub auc: f32,
    pub auprc: f32,
    pub tau: f32,
    pub tau_reject: f32,
    pub top1_agreement: Option<f32>,
}

/// Outcome of the λ sweep. Undefined metrics are reported as `0.0` (never `NaN`) so the report stays
/// JSON-representable; `top1_agreement` keeps its honest `None`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CvReport {
    pub best_lambda: f32,
    pub cv_auc: f32,
    pub cv_auprc: f32,
    /// Max-F1 threshold on `sigmoid(w·x+b)` — the "suggest pick" operating point.
    pub tau: f32,
    /// Threshold on `1 − sigmoid(w·x+b)` — the "suggest reject" operating point.
    pub tau_reject: f32,
    pub top1_agreement: Option<f32>,
    pub n_pos: usize,
    pub n_neg: usize,
    pub per_lambda: Vec<LambdaResult>,
}

/// Fold index per sample, assigned so a group is never split.
///
/// Groups are sorted by descending size and dealt round-robin, which keeps fold sizes close even when
/// one burst holds a large share of the samples.
pub(crate) fn assign_folds(samples: &[Sample], k: usize) -> Vec<usize> {
    let k = k.max(1);
    let mut sizes: BTreeMap<u64, usize> = BTreeMap::new();
    for s in samples {
        *sizes.entry(s.group).or_insert(0) += 1;
    }
    let mut order: Vec<(usize, u64)> = sizes.iter().map(|(&g, &n)| (n, g)).collect();
    // Descending size, ties broken by group id → deterministic.
    order.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let fold_of: BTreeMap<u64, usize> = order
        .iter()
        .enumerate()
        .map(|(i, &(_, g))| (g, i % k))
        .collect();
    samples.iter().map(|s| fold_of[&s.group]).collect()
}

/// Out-of-fold probability per sample. Folds with no test or no training rows are skipped (their
/// samples keep the neutral 0.5 and contribute nothing informative to the metrics).
fn oof_scores(samples: &[Sample], folds: &[usize], k: usize, cfg: &TrainConfig) -> Vec<f32> {
    let mut oof = vec![0.5f32; samples.len()];
    for f in 0..k {
        let test: Vec<usize> = (0..samples.len()).filter(|&i| folds[i] == f).collect();
        let train: Vec<Sample> = (0..samples.len())
            .filter(|&i| folds[i] != f)
            .map(|i| samples[i].clone())
            .collect();
        if test.is_empty() || train.is_empty() {
            continue;
        }
        let pairs = derive_pairs(&train);
        let r = fit(&train, &pairs, cfg);
        for &i in &test {
            oof[i] = score_with(&r.w, r.b, &r.impute_means, &samples[i].x);
        }
    }
    oof
}

fn evaluate(samples: &[Sample], oof: &[f32], lambda: f32) -> LambdaResult {
    let keep: Vec<usize> = (0..samples.len())
        .filter(|&i| samples[i].provenance.eval_ok())
        .collect();
    let scored: Vec<(f32, bool)> = keep.iter().map(|&i| (oof[i], samples[i].y)).collect();
    // Reject side: the model's confidence that this is NOT a keeper, against the flipped label.
    let inverted: Vec<(f32, bool)> = scored.iter().map(|&(p, y)| (1.0 - p, !y)).collect();
    let groups: Vec<u64> = keep.iter().map(|&i| samples[i].group).collect();
    let ys: Vec<bool> = keep.iter().map(|&i| samples[i].y).collect();
    let ss: Vec<f32> = keep.iter().map(|&i| oof[i]).collect();
    LambdaResult {
        lambda,
        auc: roc_auc(&scored).unwrap_or(0.0),
        auprc: auprc(&scored).unwrap_or(0.0),
        tau: max_f1(&scored).0,
        tau_reject: precision_threshold(&inverted, REJECT_MIN_PRECISION).unwrap_or(TAU_UNREACHABLE),
        top1_agreement: top1_agreement(&groups, &ys, &ss),
    }
}

/// K-fold sweep over `lambdas`, selecting by AUPRC (tie-break AUC).
///
/// AUPRC leads because the pick/reject prior is skewed and AUPRC tracks the minority class the UI
/// actually acts on; AUC only breaks ties.
pub fn cross_validate(
    samples: &[Sample],
    cfg_base: &TrainConfig,
    k: usize,
    lambdas: &[f32],
) -> CvReport {
    let n_groups = samples
        .iter()
        .map(|s| s.group)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let k = k.clamp(2, n_groups.max(2));
    let folds = assign_folds(samples, k);
    let grid: Vec<f32> = if lambdas.is_empty() {
        DEFAULT_LAMBDAS.to_vec()
    } else {
        lambdas.to_vec()
    };

    let per_lambda: Vec<LambdaResult> = grid
        .iter()
        .map(|&lambda| {
            let cfg = TrainConfig {
                lambda,
                ..cfg_base.clone()
            };
            evaluate(samples, &oof_scores(samples, &folds, k, &cfg), lambda)
        })
        .collect();

    let best = per_lambda
        .iter()
        .fold(None::<&LambdaResult>, |best, r| match best {
            Some(b) if (b.auprc, b.auc) >= (r.auprc, r.auc) => Some(b),
            _ => Some(r),
        })
        .cloned()
        .unwrap_or(LambdaResult {
            lambda: cfg_base.lambda,
            auc: 0.0,
            auprc: 0.0,
            tau: 0.5,
            tau_reject: TAU_UNREACHABLE,
            top1_agreement: None,
        });

    let trainable = |s: &&Sample| s.provenance != LabelProvenance::Batch;
    let n_pos = samples.iter().filter(trainable).filter(|s| s.y).count();
    let n_neg = samples.iter().filter(trainable).filter(|s| !s.y).count();
    CvReport {
        best_lambda: best.lambda,
        cv_auc: best.auc,
        cv_auprc: best.auprc,
        tau: best.tau,
        tau_reject: best.tau_reject,
        top1_agreement: best.top1_agreement,
        n_pos,
        n_neg,
        per_lambda,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{burst_samples, Lcg};
    use std::collections::HashMap;

    #[test]
    fn groups_never_span_folds() {
        let mut rng = Lcg::new(11);
        // Deliberately uneven bursts (1..=6 frames) to exercise the size-ordered round robin.
        let mut samples = Vec::new();
        for g in 0..37u64 {
            let n = 1 + (g as usize % 6);
            for i in 0..n {
                samples.push(crate::testutil::sample_x(
                    vec![rng.sym(), rng.sym()],
                    i == 0,
                    LabelProvenance::Unprompted,
                    g,
                ));
            }
        }
        let folds = assign_folds(&samples, 5);
        let mut seen: HashMap<u64, usize> = HashMap::new();
        for (s, &f) in samples.iter().zip(&folds) {
            assert!(f < 5);
            let prev = seen.entry(s.group).or_insert(f);
            assert_eq!(*prev, f, "group {} spans folds", s.group);
        }
        assert_eq!(seen.len(), 37);
        // Round-robin over 37 groups must touch every fold.
        assert_eq!(
            folds
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            5
        );
    }

    #[test]
    fn separable_data_scores_high_out_of_fold() {
        let samples = burst_samples(&mut Lcg::new(5), 24, 8, 0);
        let cfg = TrainConfig {
            iters: 400,
            ..Default::default()
        };
        let report = cross_validate(&samples, &cfg, 4, &[1e-3, 1e-2]);
        assert!(
            report.cv_auc > 0.95,
            "OOF AUC too low: {:?}",
            report.per_lambda
        );
        assert!(
            report.cv_auprc > 0.9,
            "OOF AUPRC too low: {}",
            report.cv_auprc
        );
        assert!(
            report.top1_agreement.unwrap() > 0.9,
            "burst top-1 too low: {:?}",
            report.top1_agreement
        );
        assert_eq!(report.per_lambda.len(), 2);
        assert!(report.tau > 0.0 && report.tau < 1.0);
        assert_eq!((report.n_pos, report.n_neg), (24, 48));
    }
}
