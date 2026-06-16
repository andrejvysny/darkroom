//! Error type for analyzers. Designed so a single analyzer failure is isolated (returned as `Err`)
//! and never aborts the scan pass. NOTE: the app's release profile is `panic = "abort"`, so failure
//! isolation MUST be `Result`-based — `std::panic::catch_unwind` does not work here.

#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error("model file missing: {0}")]
    ModelMissing(String),
    #[error("inference: {0}")]
    Inference(String),
    #[error("image decode: {0}")]
    Decode(String),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("download: {0}")]
    Download(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

impl AnalyzeError {
    /// Wrap any displayable error (e.g. an `ort` error) as an inference failure.
    pub fn inference(e: impl std::fmt::Display) -> Self {
        AnalyzeError::Inference(e.to_string())
    }
}
