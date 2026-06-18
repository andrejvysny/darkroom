//! Binary-classification metrics for the offline presence eval/tuning harnesses (pure math, no I/O).
//!
//! All ratios return `Option<f32>` where `None` is the explicit "n/a" sentinel for an undefined value
//! (a zero denominator) — never a silent `NaN` or `0.0`. The tuning grid-search skips `None` op points.

/// Confusion matrix for one operating point. `fn_` is "false negatives" (`fn` is a Rust keyword).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Confusion {
    pub tp: u32,
    pub fp: u32,
    pub tn: u32,
    pub fn_: u32,
}

impl Confusion {
    /// `tp / (tp + fp)` — `None` when nothing was predicted positive (precision undefined).
    pub fn precision(&self) -> Option<f32> {
        let denom = self.tp + self.fp;
        (denom > 0).then(|| self.tp as f32 / denom as f32)
    }

    /// `tp / (tp + fn)` — `None` when there are no actual positives (recall undefined).
    pub fn recall(&self) -> Option<f32> {
        let denom = self.tp + self.fn_;
        (denom > 0).then(|| self.tp as f32 / denom as f32)
    }

    /// Harmonic mean of precision and recall. `None` if either is undefined; `0.0` when there are no
    /// true positives (so `precision + recall == 0`, which would otherwise divide by zero).
    pub fn f1(&self) -> Option<f32> {
        let p = self.precision()?;
        let r = self.recall()?;
        if self.tp == 0 {
            return Some(0.0);
        }
        Some(2.0 * p * r / (p + r))
    }
}

/// Confusion matrix for predicting positive iff `score >= threshold`.
pub fn confusion_at(scores: &[(f32, bool)], threshold: f32) -> Confusion {
    let mut c = Confusion::default();
    for &(score, label) in scores {
        match (score >= threshold, label) {
            (true, true) => c.tp += 1,
            (true, false) => c.fp += 1,
            (false, true) => c.fn_ += 1,
            (false, false) => c.tn += 1,
        }
    }
    c
}

/// Area under the precision-recall curve as interpolation-free **average precision** (sklearn's
/// `average_precision_score`): rank by score descending, then `AP = Σ_positives precision_at_inclusion
/// / total_positives`. `None` when there are no positive labels (PR curve undefined).
pub fn pr_auc(scores: &[(f32, bool)]) -> Option<f32> {
    let total_pos = scores.iter().filter(|(_, y)| *y).count();
    if total_pos == 0 {
        return None;
    }
    let mut s: Vec<(f32, bool)> = scores.to_vec();
    // Descending by score; ties are vanishingly unlikely on continuous sigmoid scores.
    s.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let total_pos = total_pos as f32;
    let (mut tp, mut fp, mut ap) = (0f32, 0f32, 0f32);
    for (_, y) in s {
        if y {
            tp += 1.0;
            ap += (tp / (tp + fp)) / total_pos; // recall steps by 1/total_pos at each positive
        } else {
            fp += 1.0;
        }
    }
    Some(ap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn confusion_and_ratios_hand_checked() {
        // 0.9✓→tp, 0.8✗→fp, 0.7✓→tp, 0.2✗→tn  @0.5
        let data = [(0.9, true), (0.8, false), (0.7, true), (0.2, false)];
        let c = confusion_at(&data, 0.5);
        assert_eq!(
            c,
            Confusion {
                tp: 2,
                fp: 1,
                tn: 1,
                fn_: 0
            }
        );
        assert!(approx(c.precision().unwrap(), 2.0 / 3.0));
        assert!(approx(c.recall().unwrap(), 1.0));
        assert!(approx(c.f1().unwrap(), 0.8)); // 2·(2/3)·1 / (2/3 + 1)
    }

    #[test]
    fn degenerate_no_predicted_positives_is_na_not_div0() {
        // All-positive labels, threshold above every score → nothing predicted positive.
        let data = [(0.4, true), (0.3, true), (0.1, true)];
        let c = confusion_at(&data, 0.9);
        assert_eq!(c.tp + c.fp, 0);
        assert_eq!(c.precision(), None); // n/a sentinel, not a divide-by-zero
        assert_eq!(c.recall(), Some(0.0)); // there ARE positives, none recovered
        assert_eq!(c.f1(), None); // undefined because precision is undefined
    }

    #[test]
    fn pr_auc_perfect_ranking_is_one() {
        let data = [(0.9, true), (0.8, true), (0.7, false)];
        assert!(approx(pr_auc(&data).unwrap(), 1.0));
    }

    #[test]
    fn pr_auc_no_positives_is_na() {
        assert_eq!(pr_auc(&[(0.9, false), (0.1, false)]), None);
    }
}
