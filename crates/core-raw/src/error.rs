use thiserror::Error;

#[derive(Debug, Error)]
pub enum RawError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(String),
    #[error("no embedded preview/thumbnail found")]
    NoPreview,
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
}
