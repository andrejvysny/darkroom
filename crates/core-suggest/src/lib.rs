//! core-suggest — on-device pick/reject preference learning.
//!
//! Pure math: no DB, no ONNX, no GPU, no I/O. The caller supplies an image embedding plus a handful of
//! computed capture/quality signals per labeled image; this crate turns those into a small logistic
//! head that predicts *this user's* keeper, and reports honestly how well it does.
//!
//! Three ideas do the real work:
//!
//! - **Provenance-weighted labels** ([`LabelProvenance`]). A pick the user made with no suggestion on
//!   screen is worth far more than one that merely agreed with a suggestion, and agreements are capped
//!   at [`AGREE_MASS_CAP`] of the total training mass so the model cannot bootstrap itself into its own
//!   prior. Bulk actions train on nothing.
//! - **Pointwise + pairwise objective** ([`fit`]). BCE learns "is this a keeper"; a Bradley-Terry term
//!   over within-burst (pick, reject) pairs learns "is this *the* keeper of this burst" — the question
//!   the UI actually asks.
//! - **Group-aware CV** ([`cross_validate`]). Burst siblings are near-duplicates, so folds split by
//!   burst, and metrics are computed only on labels the model could not have influenced.
//!
//! ```no_run
//! use core_suggest::{assemble, train, HandFeatures, LabelProvenance, Sample};
//! # fn go(emb: &[f32]) -> Result<(), core_suggest::SuggestError> {
//! let x = assemble(emb, &HandFeatures { sharpness_log: 3.2, ..Default::default() })?;
//! let samples = vec![Sample { x, y: true, provenance: LabelProvenance::Unprompted, group: 1 }];
//! let (model, report) = train(&samples, 5, &[], None, "mobileclip-s1", 0)?;
//! let p = model.score(emb, &HandFeatures::default())?;
//! let suggest_pick = p >= model.tau;
//! let suggest_reject = 1.0 - p >= model.tau_reject;
//! # let _ = (report, suggest_pick, suggest_reject);
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod cv;
mod error;
mod features;
mod fit;
pub mod metrics;
mod model;
mod sample;
#[cfg(test)]
mod testutil;
mod weights;

pub use cv::{
    cross_validate, CvReport, LambdaResult, DEFAULT_LAMBDAS, REJECT_MIN_PRECISION, TAU_UNREACHABLE,
};
pub use error::SuggestError;
pub use features::{assemble, HandFeatures, DIM, EMB_DIM, FEATURE_VERSION, HAND_DIM};
pub use fit::{fit, FitResult, TrainConfig};
pub use model::{train, Model, MIN_PER_CLASS, MODEL_SCHEMA};
pub use sample::{LabelProvenance, Pair, Sample};
pub use weights::{compute_weights, derive_pairs, AGREE_MASS_CAP};
