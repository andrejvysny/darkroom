use thiserror::Error;

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error("no compatible GPU adapter")]
    NoAdapter,
    #[error("request device: {0}")]
    Device(String),
    #[error("image {w}x{h} exceeds GPU max texture dimension {max}")]
    ImageTooLarge { w: u32, h: u32, max: u32 },
    #[error("invalid image input: {0}")]
    InvalidInput(&'static str),
    #[error("image readback needs {required} bytes, exceeding GPU buffer limit {max}")]
    BufferTooLarge { required: u64, max: u64 },
    #[error("image allocation: {0}")]
    Allocation(String),
    #[error("invalid develop render target: {0}")]
    InvalidTarget(String),
    #[error("buffer map: {0}")]
    Map(String),
    #[error("raw: {0}")]
    Raw(#[from] core_raw::RawError),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
}
