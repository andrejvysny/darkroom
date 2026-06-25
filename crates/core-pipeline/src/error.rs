use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("no compatible GPU adapter")]
    NoAdapter,
    #[error("request device: {0}")]
    Device(String),
    #[error("image {w}x{h} exceeds GPU max texture dimension {max}")]
    ImageTooLarge { w: u32, h: u32, max: u32 },
    #[error("buffer map: {0}")]
    Map(String),
    #[error("raw: {0}")]
    Raw(#[from] core_raw::RawError),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
}
