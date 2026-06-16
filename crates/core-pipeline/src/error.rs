use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("no compatible GPU adapter")]
    NoAdapter,
    #[error("request device: {0}")]
    Device(String),
    #[error("buffer map: {0}")]
    Map(String),
    #[error("raw: {0}")]
    Raw(#[from] core_raw::RawError),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
}
