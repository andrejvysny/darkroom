//! Sample weighting and pair derivation.
//!
//! Final weight = provenance trust × inverse-frequency class balance, then a hard cap on how much of
//! the total training mass may come from "the user agreed with me" labels. Without the cap a long
//! run of agreements would let the model reinforce itself into whatever it already believed, and the
//! rare unprompted/override labels — the only ones carrying new information — would stop mattering.

use std::collections::BTreeMap;

use crate::sample::{LabelProvenance, Pair, Sample};

/// Maximum share of the total weight mass allowed to come from `AgreeLo` + `AgreeHi` labels.
pub const AGREE_MASS_CAP: f32 = 0.30;

/// Per-sample training weights, aligned to `samples`.
///
/// Class balance uses the same normalization as the presence probe's `fit()` (`cw_pos = n/(2·n_pos)`,
/// mean weight ≈ 1), computed over **non-`Batch`** samples only — `Batch` rows are weight 0 and must
/// not shift the class prior. When the agree mass exceeds [`AGREE_MASS_CAP`] the agree rows are scaled
/// so it lands at exactly the cap.
pub fn compute_weights(samples: &[Sample]) -> Vec<f32> {
    let trainable = |s: &Sample| s.provenance != LabelProvenance::Batch;
    let n = samples.iter().filter(|s| trainable(s)).count();
    let n_pos = samples.iter().filter(|s| trainable(s) && s.y).count();
    let n_neg = n - n_pos;
    let cw_pos = n as f32 / (2.0 * n_pos.max(1) as f32);
    let cw_neg = n as f32 / (2.0 * n_neg.max(1) as f32);

    let mut w: Vec<f32> = samples
        .iter()
        .map(|s| s.provenance.base_weight() * if s.y { cw_pos } else { cw_neg })
        .collect();
    apply_agree_cap(samples, &mut w);
    w
}

/// Scale the agree rows so their share of the total mass is exactly [`AGREE_MASS_CAP`].
///
/// Solves `A' = cap·(A' + R)` for the rest-mass `R`, i.e. `A' = R·cap/(1−cap)`. Skipped when `R == 0`
/// (an all-agree set) — capping there would zero every weight and produce a model of nothing; that
/// data is degenerate and is rejected upstream instead.
fn apply_agree_cap(samples: &[Sample], w: &mut [f32]) {
    let mut agree = 0f64;
    let mut rest = 0f64;
    for (s, &wi) in samples.iter().zip(w.iter()) {
        if s.provenance.is_agree() {
            agree += wi as f64;
        } else {
            rest += wi as f64;
        }
    }
    let cap = AGREE_MASS_CAP as f64;
    if rest <= 0.0 || agree <= 0.0 || agree <= cap * (agree + rest) {
        return;
    }
    let scale = (rest * cap / (1.0 - cap)) / agree;
    for (s, wi) in samples.iter().zip(w.iter_mut()) {
        if s.provenance.is_agree() {
            *wi = (*wi as f64 * scale) as f32;
        }
    }
}

/// Within-burst preference pairs: every (pick, reject) combination inside a group.
///
/// Pair weight is the geometric mean of the two samples' final weights, so a pair is only as
/// trustworthy as its weaker label. `Batch` samples are excluded entirely (they train on nothing,
/// pointwise or pairwise). Ordering is deterministic (groups ascending, then index ascending).
pub fn derive_pairs(samples: &[Sample]) -> Vec<Pair> {
    let w = compute_weights(samples);
    let mut by_group: BTreeMap<u64, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (i, s) in samples.iter().enumerate() {
        if s.provenance == LabelProvenance::Batch {
            continue;
        }
        let entry = by_group.entry(s.group).or_default();
        if s.y {
            entry.0.push(i);
        } else {
            entry.1.push(i);
        }
    }
    let mut pairs = Vec::new();
    for (picks, rejects) in by_group.values() {
        for &winner in picks {
            for &loser in rejects {
                let weight = (w[winner] as f64 * w[loser] as f64).sqrt() as f32;
                if weight > 0.0 {
                    pairs.push(Pair {
                        winner,
                        loser,
                        weight,
                    });
                }
            }
        }
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::sample;

    fn mass(samples: &[Sample], w: &[f32], agree: bool) -> f64 {
        samples
            .iter()
            .zip(w)
            .filter(|(s, _)| s.provenance.is_agree() == agree)
            .map(|(_, &wi)| wi as f64)
            .sum()
    }

    #[test]
    fn agree_mass_is_capped_at_thirty_percent() {
        // 4 unprompted + 16 agree-lo: raw agree share = 13.6/17.6 ≈ 77%.
        let mut s = Vec::new();
        for i in 0..4 {
            s.push(sample(i % 2 == 0, LabelProvenance::Unprompted, i as u64));
        }
        for i in 0..16 {
            s.push(sample(i % 2 == 0, LabelProvenance::AgreeLo, 100 + i as u64));
        }
        let w = compute_weights(&s);
        let (a, r) = (mass(&s, &w, true), mass(&s, &w, false));
        assert!(a > 0.0 && r > 0.0, "both masses must survive the cap");
        assert!(
            a / (a + r) <= AGREE_MASS_CAP as f64 + 1e-5,
            "agree share {} exceeds cap",
            a / (a + r)
        );
        assert!(
            (a / (a + r) - AGREE_MASS_CAP as f64).abs() < 1e-5,
            "cap is exact"
        );
    }

    #[test]
    fn under_cap_weights_are_untouched_and_batch_is_zero() {
        let s = vec![
            sample(true, LabelProvenance::Unprompted, 0),
            sample(false, LabelProvenance::Unprompted, 0),
            sample(true, LabelProvenance::AgreeHi, 1),
            sample(false, LabelProvenance::Batch, 2),
        ];
        let w = compute_weights(&s);
        // n = 3 non-batch (2 pos, 1 neg) → cw_pos = 0.75, cw_neg = 1.5.
        assert!((w[0] - 0.75).abs() < 1e-6);
        assert!((w[1] - 1.5).abs() < 1e-6);
        assert!((w[2] - 0.2 * 0.75).abs() < 1e-6);
        assert_eq!(w[3], 0.0);
    }

    #[test]
    fn pairs_are_group_scoped_cross_products_skipping_batch() {
        let s = vec![
            sample(true, LabelProvenance::Unprompted, 7),
            sample(false, LabelProvenance::Unprompted, 7),
            sample(false, LabelProvenance::Unprompted, 7),
            sample(false, LabelProvenance::Batch, 7),
            sample(true, LabelProvenance::Unprompted, 8), // different burst → no cross-group pair
        ];
        let pairs = derive_pairs(&s);
        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().all(|p| p.winner == 0));
        assert_eq!(
            pairs.iter().map(|p| p.loser).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let w = compute_weights(&s);
        assert!((pairs[0].weight - (w[0] * w[1]).sqrt()).abs() < 1e-6);
    }
}
