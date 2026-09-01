//! Error type for the preference model. Every failure is a *contract* failure (wrong dimensions,
//! stale feature layout, not enough labels) — there is no I/O in this crate, so nothing here is
//! transient or retryable.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SuggestError {
    #[error("dimension mismatch: expected {expected}, got {got}")]
    DimMismatch { expected: usize, got: usize },
    /// The stored model was fit on a different feature layout — its weights are meaningless against
    /// today's [`crate::HandFeatures`] order. Retrain, never coerce.
    #[error("feature version mismatch: model {model}, runtime {runtime}")]
    FeatureVersionMismatch { model: u32, runtime: u32 },
    #[error("too few labeled samples: need {need} per class, got {got}")]
    TooFewSamples { need: usize, got: usize },
    #[error("degenerate training data: {0}")]
    DegenerateData(&'static str),
}
