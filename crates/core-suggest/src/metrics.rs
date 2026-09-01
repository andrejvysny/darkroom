//! Ranking / classification metrics over `(score, label)` pairs. Pure math, no I/O.
//!
//! Every ratio that can be undefined returns `Option` — a zero denominator is reported as `None`,
//! never silently as `0.0` or `NaN`, so an honest CV report can say "n/a" instead of inventing a
//! number (same contract as `core-analyze::metrics`).

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::sample::Sample;

fn cmp_desc(a: f32, b: f32) -> Ordering {
    b.partial_cmp(&a).unwrap_or(Ordering::Equal)
}

/// ROC-AUC via the Mann-Whitney U statistic (mean rank of positives), tie-aware. `None` if either
/// class is empty.
pub fn roc_auc(scored: &[(f32, bool)]) -> Option<f32> {
    let n_pos = scored.iter().filter(|(_, y)| *y).count();
    let n_neg = scored.len() - n_pos;
    if n_pos == 0 || n_neg == 0 {
        return None;
    }
    let mut idx: Vec<usize> = (0..scored.len()).collect();
    idx.sort_by(|&a, &b| {
        scored[a]
            .0
            .partial_cmp(&scored[b].0)
            .unwrap_or(Ordering::Equal)
    });
    // Average ranks (1-based); a tied run shares the mean of the ranks it spans.
    let mut ranks = vec![0f64; scored.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i + 1;
        while j < idx.len() && scored[idx[j]].0.to_bits() == scored[idx[i]].0.to_bits() {
            j += 1;
        }
        let avg = ((i + 1 + j) as f64) / 2.0;
        for &k in &idx[i..j] {
            ranks[k] = avg;
        }
        i = j;
    }
    let sum_pos: f64 = scored
        .iter()
        .zip(&ranks)
        .filter(|((_, y), _)| *y)
        .map(|(_, r)| *r)
        .sum();
    let u = sum_pos - (n_pos * (n_pos + 1)) as f64 / 2.0;
    Some((u / (n_pos as f64 * n_neg as f64)) as f32)
}

/// Area under the precision-recall curve by step-wise integration: rank descending, and at every
/// distinct-score boundary add `precision · Δrecall`. Ties are consumed as one block (a threshold can
/// only fall *between* distinct scores). `None` when there are no positives.
pub fn auprc(scored: &[(f32, bool)]) -> Option<f32> {
    let n_pos = scored.iter().filter(|(_, y)| *y).count();
    if n_pos == 0 {
        return None;
    }
    let mut s: Vec<(f32, bool)> = scored.to_vec();
    s.sort_by(|a, b| cmp_desc(a.0, b.0));
    let (mut tp, mut fp, mut ap, mut prev_recall) = (0f64, 0f64, 0f64, 0f64);
    let mut i = 0;
    while i < s.len() {
        let mut j = i;
        while j < s.len() && s[j].0.to_bits() == s[i].0.to_bits() {
            if s[j].1 {
                tp += 1.0;
            } else {
                fp += 1.0;
            }
            j += 1;
        }
        let recall = tp / n_pos as f64;
        ap += (tp / (tp + fp)) * (recall - prev_recall);
        prev_recall = recall;
        i = j;
    }
    Some(ap as f32)
}

/// `(tp, fp, fn)` for predicting positive iff `score >= threshold`.
fn confusion_at(scored: &[(f32, bool)], threshold: f32) -> (u32, u32, u32) {
    let (mut tp, mut fp, mut fneg) = (0u32, 0u32, 0u32);
    for &(score, label) in scored {
        match (score >= threshold, label) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, true) => fneg += 1,
            (false, false) => {}
        }
    }
    (tp, fp, fneg)
}

fn f1_at(scored: &[(f32, bool)], threshold: f32) -> f32 {
    let (tp, fp, fneg) = confusion_at(scored, threshold);
    if tp == 0 {
        return 0.0;
    }
    let p = tp as f32 / (tp + fp) as f32;
    let r = tp as f32 / (tp + fneg) as f32;
    2.0 * p * r / (p + r)
}

/// Max-F1 probability threshold, swept over `0.01..=0.99` → `(tau, f1)`. Defaults to `(0.5, 0.0)`
/// when no threshold produces a true positive.
pub fn max_f1(scored: &[(f32, bool)]) -> (f32, f32) {
    let mut best = (0.5f32, 0f32);
    for i in 1..100 {
        let tau = i as f32 / 100.0;
        let f1 = f1_at(scored, tau);
        if f1 > best.1 {
            best = (tau, f1);
        }
    }
    best
}

/// Lowest threshold whose precision is at least `min_precision` — equivalently the highest-recall
/// operating point that still meets the precision floor, since recall is non-increasing in `τ`.
/// `None` when no threshold reaches that precision.
///
/// Candidates are the observed scores themselves (predict positive iff `score >= τ`), so the returned
/// `τ` is exactly achievable rather than snapped to a grid.
pub fn precision_threshold(scored: &[(f32, bool)], min_precision: f32) -> Option<f32> {
    let mut s: Vec<(f32, bool)> = scored.to_vec();
    s.sort_by(|a, b| cmp_desc(a.0, b.0));
    let (mut tp, mut fp) = (0f64, 0f64);
    let mut best = None;
    let mut i = 0;
    while i < s.len() {
        let mut j = i;
        while j < s.len() && s[j].0.to_bits() == s[i].0.to_bits() {
            if s[j].1 {
                tp += 1.0;
            } else {
                fp += 1.0;
            }
            j += 1;
        }
        // Descending sweep: every later qualifying boundary is a lower τ (more recall).
        if tp + fp > 0.0 && (tp / (tp + fp)) as f32 >= min_precision {
            best = Some(s[i].0);
        }
        i = j;
    }
    best
}

/// Fraction of multi-image bursts (that contain at least one pick) whose highest-scored image is a
/// pick — the metric that actually matches the product ("suggest the keeper of this burst").
/// `None` when there is no such burst.
pub fn burst_top1_agreement(samples: &[Sample], scores: &[f32]) -> Option<f32> {
    let groups: Vec<u64> = samples.iter().map(|s| s.group).collect();
    let y: Vec<bool> = samples.iter().map(|s| s.y).collect();
    top1_agreement(&groups, &y, scores)
}

/// Borrowed-column form of [`burst_top1_agreement`] (CV works over filtered index views and must not
/// clone every 528-wide feature row just to group them).
pub(crate) fn top1_agreement(groups: &[u64], y: &[bool], scores: &[f32]) -> Option<f32> {
    if groups.len() != y.len() || groups.len() != scores.len() {
        return None;
    }
    let mut by_group: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    for (i, g) in groups.iter().enumerate() {
        by_group.entry(*g).or_default().push(i);
    }
    let (mut hit, mut total) = (0usize, 0usize);
    for idx in by_group.values() {
        if idx.len() < 2 || !idx.iter().any(|&i| y[i]) {
            continue;
        }
        let mut best = idx[0];
        for &i in &idx[1..] {
            if scores[i] > scores[best] {
                best = i;
            }
        }
        total += 1;
        hit += usize::from(y[best]);
    }
    (total > 0).then(|| hit as f32 / total as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::LabelProvenance;
    use crate::testutil::sample;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn roc_auc_perfect_and_tied() {
        assert!(approx(
            roc_auc(&[(0.9, true), (0.8, true), (0.7, false)]).unwrap(),
            1.0
        ));
        // All scores tied → every ordering equally likely → 0.5.
        assert!(approx(
            roc_auc(&[(0.5, true), (0.5, false), (0.5, true), (0.5, false)]).unwrap(),
            0.5
        ));
        assert_eq!(roc_auc(&[(0.9, true)]), None);
    }

    #[test]
    fn auprc_perfect_and_worst() {
        assert!(approx(
            auprc(&[(0.9, true), (0.8, true), (0.7, false)]).unwrap(),
            1.0
        ));
        // Single positive ranked last of three: precision 1/3 at recall 1 → AP = 1/3.
        assert!(approx(
            auprc(&[(0.9, false), (0.8, false), (0.7, true)]).unwrap(),
            1.0 / 3.0
        ));
        assert_eq!(auprc(&[(0.1, false)]), None);
    }

    #[test]
    fn max_f1_picks_the_separating_threshold() {
        let (tau, f1) = max_f1(&[(0.9, true), (0.8, true), (0.2, false), (0.1, false)]);
        assert!(approx(f1, 1.0));
        assert!(tau > 0.2 && tau <= 0.8);
    }

    #[test]
    fn precision_threshold_picks_lowest_qualifying_tau() {
        // τ=0.9 → 1/1, τ=0.8 → 2/2, τ=0.7 → 2/3, τ=0.6 → 3/4, τ=0.5 → 3/5.
        let data = [
            (0.9, true),
            (0.8, true),
            (0.7, false),
            (0.6, true),
            (0.5, false),
        ];
        assert_eq!(precision_threshold(&data, 0.75), Some(0.6));
        assert_eq!(precision_threshold(&data, 0.9), Some(0.8));
        // Unattainable: the top-scored item is a negative, so no τ reaches precision 1.0.
        assert_eq!(precision_threshold(&[(0.9, false), (0.8, true)], 1.0), None);
    }

    #[test]
    fn precision_threshold_respects_ties() {
        // The two 0.8s share a threshold: precision there is 1/2, never 1/1.
        let data = [(0.8, true), (0.8, false), (0.1, true)];
        assert_eq!(precision_threshold(&data, 1.0), None);
        assert_eq!(precision_threshold(&data, 0.5), Some(0.1)); // 2/3 ≥ 0.5, lower τ wins
    }

    #[test]
    fn burst_top1_agreement_hand_case() {
        let p = LabelProvenance::Unprompted;
        let s = vec![
            sample(true, p, 1), // burst 1: pick scores highest → hit
            sample(false, p, 1),
            sample(true, p, 2), // burst 2: reject scores highest → miss
            sample(false, p, 2),
            sample(true, p, 3),  // singleton → ignored
            sample(false, p, 4), // burst 4 has no pick → ignored
            sample(false, p, 4),
        ];
        let scores = [0.9, 0.1, 0.2, 0.8, 0.99, 0.7, 0.6];
        assert_eq!(burst_top1_agreement(&s, &scores), Some(0.5));
        assert_eq!(burst_top1_agreement(&s[4..5], &scores[4..5]), None);
        assert_eq!(burst_top1_agreement(&s, &scores[..3]), None); // length guard
    }
}
