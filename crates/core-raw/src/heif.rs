//! HDR PQ HEIF (`.hif`, Canon 10-bit BT.2020/ST-2084) decode. Placeholder — lands in Phase 2.

use crate::develop::LinearImage;
use crate::error::RawError;
use crate::meta::RawMeta;
use crate::thumb::Thumb;
use image::DynamicImage;

fn todo() -> RawError {
    RawError::Decode("HEIF (.HIF) decode lands in Phase 2".into())
}

pub(crate) fn decode_heif_linear(_bytes: &[u8]) -> Result<LinearImage, RawError> {
    Err(todo())
}

pub(crate) fn decode_heif_preview(_bytes: &[u8]) -> Result<DynamicImage, RawError> {
    Err(todo())
}

pub(crate) fn decode_heif_thumb(
    _bytes: &[u8],
    _max_edge: u32,
    _quality: u8,
) -> Result<Thumb, RawError> {
    Err(todo())
}

pub(crate) fn read_heif_meta(_bytes: &[u8]) -> RawMeta {
    RawMeta::default()
}
