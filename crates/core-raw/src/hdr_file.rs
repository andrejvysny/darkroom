//! Merged-HDR OpenEXR (`.exr`, linear ProPhoto fp16) read/write. Placeholder — lands in Phase 3.

use crate::develop::LinearImage;
use crate::error::RawError;
use crate::meta::RawMeta;
use crate::thumb::Thumb;
use image::DynamicImage;

fn todo() -> RawError {
    RawError::Decode("merged-HDR EXR decode lands in Phase 3".into())
}

pub(crate) fn read_hdr_linear(_bytes: &[u8]) -> Result<LinearImage, RawError> {
    Err(todo())
}

pub(crate) fn decode_hdr_preview(_bytes: &[u8]) -> Result<DynamicImage, RawError> {
    Err(todo())
}

pub(crate) fn decode_hdr_thumb(
    _bytes: &[u8],
    _max_edge: u32,
    _quality: u8,
) -> Result<Thumb, RawError> {
    Err(todo())
}

pub(crate) fn read_hdr_meta(_bytes: &[u8]) -> RawMeta {
    RawMeta::default()
}
