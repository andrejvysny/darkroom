//! The persisted model and the end-to-end training entry point.

use serde::{Deserialize, Serialize};

use crate::cv::{cross_validate, CvReport, DEFAULT_LAMBDAS};
use crate::error::SuggestError;
use crate::features::{assemble, HandFeatures, DIM, EMB_DIM, FEATURE_VERSION};
use crate::fit::{fit, score_with, TrainConfig};
use crate::sample::{LabelProvenance, Sample};
use crate::weights::{compute_weights, derive_pairs};

/// On-disk schema of [`Model`]. Bumped when the JSON shape changes (as opposed to
/// [`FEATURE_VERSION`], which versions the *input* layout).
pub const MODEL_SCHEMA: u32 = 1;

/// Labels required per class before a model is worth fitting at all.
pub const MIN_PER_CLASS: usize = 10;

/// A trained preference head plus everything needed to score and to judge it honestly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Model {
    pub schema: u32,
    pub feature_version: u32,
    pub emb_dim: usize,
    pub dim: usize,
    /// Raw-space weights (standardization already folded in).
    pub w: Vec<f32>,
    pub b: f32,
    /// Training-set column means used to fill missing features — scoring MUST reuse them.
    pub impute_means: Vec<f32>,
    /// Max-F1 threshold for "suggest pick" (out-of-fold).
    pub tau: f32,
    /// Precision-floored threshold on `1 − p` for "suggest reject" (out-of-fold).
    pub tau_reject: f32,
    pub trained_at_ms: i64,
    pub n_pos: usize,
    pub n_neg: usize,
    pub cv_auc: f32,
    pub cv_auprc: f32,
    pub top1_agreement: Option<f32>,
    /// Which embedding produced the first [`EMB_DIM`] features — weights are meaningless against a
    /// different encoder even at the same width.
    pub embedding_model_tag: String,
}

impl Model {
    /// `sigmoid(w·[emb ‖ hand] + b)`, with missing hand features filled from [`Model::impute_means`].
    pub fn score(&self, emb: &[f32], hand: &HandFeatures) -> Result<f32, SuggestError> {
        if self.feature_version != FEATURE_VERSION {
            return Err(SuggestError::FeatureVersionMismatch {
                model: self.feature_version,
                runtime: FEATURE_VERSION,
            });
        }
        if self.emb_dim != EMB_DIM {
            return Err(SuggestError::DimMismatch {
                expected: EMB_DIM,
                got: self.emb_dim,
            });
        }
        for got in [self.dim, self.w.len(), self.impute_means.len()] {
            if got != DIM {
                return Err(SuggestError::DimMismatch { expected: DIM, got });
            }
        }
        let x = assemble(emb, hand)?;
        Ok(score_with(&self.w, self.b, &self.impute_means, &x))
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// Fit a preference model: group-aware CV to choose λ and the two thresholds, then a final fit on
/// **all** samples at that λ.
///
/// The final fit deliberately uses every sample (including the agree mass, capped) while the reported
/// metrics come only from out-of-fold, uninfluenced labels — the model is as informed as possible and
/// the numbers next to it are still honest. `warm` seeds GD from the previous model when its layout
/// matches, so a nightly retrain is a nudge rather than a restart.
pub fn train(
    samples: &[Sample],
    k: usize,
    lambdas: &[f32],
    warm: Option<&Model>,
    embedding_model_tag: &str,
    trained_at_ms: i64,
) -> Result<(Model, CvReport), SuggestError> {
    if let Some(bad) = samples.iter().find(|s| s.x.len() != DIM) {
        return Err(SuggestError::DimMismatch {
            expected: DIM,
            got: bad.x.len(),
        });
    }
    let trainable = |s: &&Sample| s.provenance != LabelProvenance::Batch;
    let n_pos = samples.iter().filter(trainable).filter(|s| s.y).count();
    let n_neg = samples.iter().filter(trainable).filter(|s| !s.y).count();
    if n_pos < MIN_PER_CLASS || n_neg < MIN_PER_CLASS {
        return Err(SuggestError::TooFewSamples {
            need: MIN_PER_CLASS,
            got: n_pos.min(n_neg),
        });
    }
    if compute_weights(samples).iter().all(|&w| w <= 0.0) {
        return Err(SuggestError::DegenerateData("every sample weight is zero"));
    }

    let base = TrainConfig::default();
    let grid: &[f32] = if lambdas.is_empty() {
        &DEFAULT_LAMBDAS
    } else {
        lambdas
    };
    let report = cross_validate(samples, &base, k, grid);

    let warm_start = warm
        .filter(|m| m.feature_version == FEATURE_VERSION && m.dim == DIM && m.w.len() == DIM)
        .map(|m| {
            let mut v = m.w.clone();
            v.push(m.b);
            v
        });
    let cfg = TrainConfig {
        lambda: report.best_lambda,
        warm_start,
        ..base
    };
    let fitted = fit(samples, &derive_pairs(samples), &cfg);

    let model = Model {
        schema: MODEL_SCHEMA,
        feature_version: FEATURE_VERSION,
        emb_dim: EMB_DIM,
        dim: DIM,
        w: fitted.w,
        b: fitted.b,
        impute_means: fitted.impute_means,
        tau: report.tau,
        tau_reject: report.tau_reject,
        trained_at_ms,
        n_pos,
        n_neg,
        cv_auc: report.cv_auc,
        cv_auprc: report.cv_auprc,
        top1_agreement: report.top1_agreement,
        embedding_model_tag: embedding_model_tag.to_string(),
    };
    Ok((model, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{burst_samples, full_hand, Lcg};

    fn hand_model() -> Model {
        let mut rng = Lcg::new(99);
        Model {
            schema: MODEL_SCHEMA,
            feature_version: FEATURE_VERSION,
            emb_dim: EMB_DIM,
            dim: DIM,
            // Small weights keep the 528-term logit off the sigmoid's saturated tails, so a
            // one-feature change is still visible in the score.
            w: (0..DIM).map(|_| rng.sym() * 0.01).collect(),
            b: 0.13,
            impute_means: (0..DIM).map(|_| rng.sym() * 2.0).collect(),
            tau: 0.61,
            tau_reject: 0.88,
            trained_at_ms: 1_754_000_000_000,
            n_pos: 31,
            n_neg: 77,
            cv_auc: 0.812,
            cv_auprc: 0.744,
            top1_agreement: Some(0.7),
            embedding_model_tag: "mobileclip-s1".to_string(),
        }
    }

    #[test]
    fn serde_round_trip_preserves_scores() {
        let m = hand_model();
        let back = Model::from_json(&m.to_json().unwrap()).unwrap();
        let emb: Vec<f32> = (0..EMB_DIM).map(|i| (i as f32 * 0.01).sin()).collect();
        let hand = full_hand();
        assert_eq!(
            m.score(&emb, &hand).unwrap(),
            back.score(&emb, &hand).unwrap()
        );
        assert_eq!(back.tau, m.tau);
        assert_eq!(back.top1_agreement, m.top1_agreement);
        assert_eq!(back.embedding_model_tag, m.embedding_model_tag);
    }

    #[test]
    fn missing_feature_scores_as_the_stored_mean() {
        let m = hand_model();
        let emb: Vec<f32> = (0..EMB_DIM).map(|i| (i as f32 * 0.003).cos()).collect();
        let mut nan_hand = full_hand();
        nan_hand.face_max_quality = f32::NAN;
        nan_hand.log_iso = f32::NAN;
        let mut filled = nan_hand;
        // Hand features occupy columns EMB_DIM.. in HandFeatures::to_vec order (7 and 9 here).
        filled.face_max_quality = m.impute_means[EMB_DIM + 7];
        filled.log_iso = m.impute_means[EMB_DIM + 9];
        assert_eq!(
            m.score(&emb, &nan_hand).unwrap(),
            m.score(&emb, &filled).unwrap()
        );
        assert_ne!(
            m.score(&emb, &nan_hand).unwrap(),
            m.score(&emb, &full_hand()).unwrap()
        );
    }

    #[test]
    fn version_and_dim_mismatch_are_rejected() {
        let emb = vec![0.0f32; EMB_DIM];
        let hand = full_hand();
        let mut stale = hand_model();
        stale.feature_version = FEATURE_VERSION + 1;
        assert_eq!(
            stale.score(&emb, &hand),
            Err(SuggestError::FeatureVersionMismatch {
                model: FEATURE_VERSION + 1,
                runtime: FEATURE_VERSION
            })
        );
        let mut truncated = hand_model();
        truncated.w.pop();
        assert_eq!(
            truncated.score(&emb, &hand),
            Err(SuggestError::DimMismatch {
                expected: DIM,
                got: DIM - 1
            })
        );
        assert_eq!(
            hand_model().score(&[0.0; 4], &hand),
            Err(SuggestError::DimMismatch {
                expected: EMB_DIM,
                got: 4
            })
        );
    }

    #[test]
    fn train_rejects_thin_and_ragged_input() {
        let thin = burst_samples(&mut Lcg::new(1), 3, DIM, EMB_DIM);
        assert_eq!(
            train(&thin, 3, &[1e-2], None, "tag", 0).unwrap_err(),
            SuggestError::TooFewSamples { need: 10, got: 3 }
        );
        let ragged = burst_samples(&mut Lcg::new(1), 12, 8, 0);
        assert_eq!(
            train(&ragged, 3, &[1e-2], None, "tag", 0).unwrap_err(),
            SuggestError::DimMismatch {
                expected: DIM,
                got: 8
            }
        );
    }

    #[test]
    fn train_builds_a_scoreable_model_and_warm_starts_from_it() {
        let samples = burst_samples(&mut Lcg::new(3), 12, DIM, EMB_DIM);
        let (model, report) = train(&samples, 2, &[1e-2], None, "mobileclip-s1", 42).unwrap();
        assert_eq!((model.schema, model.dim, model.emb_dim), (1, DIM, EMB_DIM));
        assert_eq!((model.n_pos, model.n_neg), (12, 24));
        assert_eq!(model.trained_at_ms, 42);
        assert_eq!(model.cv_auc, report.cv_auc);
        assert!(report.cv_auc > 0.9, "OOF AUC {}", report.cv_auc);

        // A pick-like sample must outscore a reject-like one (signal is hand column 0).
        let emb = vec![0.0f32; EMB_DIM];
        let mut good = full_hand();
        let mut bad = full_hand();
        good.sharpness_log = 1.0;
        bad.sharpness_log = -1.0;
        assert!(model.score(&emb, &good).unwrap() > model.score(&emb, &bad).unwrap());

        let (warmed, _) = train(&samples, 2, &[1e-2], Some(&model), "mobileclip-s1", 43).unwrap();
        assert!(warmed.score(&emb, &good).unwrap() > warmed.score(&emb, &bad).unwrap());
    }
}
