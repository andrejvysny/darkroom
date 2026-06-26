use thiserror::Error;

/// Errors raised while importing an external preset file.
#[derive(Debug, Error)]
pub enum PresetError {
    /// No registered importer recognized the bytes/extension.
    #[error("unrecognized preset format")]
    UnknownFormat,
    /// The file matched a format but could not be parsed.
    #[error("malformed preset: {0}")]
    Malformed(String),
    /// Underlying I/O failure (surfaced as a string to stay `Send + Sync` and dependency-light).
    #[error("io error: {0}")]
    Io(String),
}
