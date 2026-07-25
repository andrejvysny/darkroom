//! core-raw — RAW decode, embedded preview/thumbnail extraction, metadata, and identity hashing.
//!
//! All `rawler` calls are isolated in this crate (rawler's API is non-SemVer; pinned `=0.7.2`).

pub mod color;
pub mod develop;
pub mod display;
pub mod error;
pub mod hash;
pub mod hdr_dng;
pub mod hdr_file;
pub mod heif;
pub mod meta;
pub mod pano;
pub mod thumb;

pub use color::HDR_DIFFUSE_WHITE_NITS;
pub use develop::{
    as_shot_wb, develop_linear, develop_linear_denoised, develop_linear_preview, develop_linear_wb,
    DenoiseOutput, LinearImage, MosaicDenoiser, MosaicInfo,
};
pub use display::{classify, is_display, ImageKind};
pub use error::RawError;
pub use hash::{content_hash, hash_file, hex};
pub use hdr_dng::write_hdr_dng;
pub use hdr_file::{read_hdr_sources, write_hdr_exr, HdrSourceInfo, HdrSources};
pub use meta::{capture_fingerprint, read_exposure_numeric, read_metadata, RawMeta};
pub use pano::{
    develop_camera_native, native_to_srgb_jpeg, write_pano_dng, CameraNativeImage, PanoColorMeta,
};
pub use thumb::{oriented_preview, preview_image, preview_with_orientation, thumbnail_jpeg, Thumb};

pub use rawler::rawsource::RawSource;

use std::path::Path;
use std::sync::Arc;

/// Build a [`RawSource`] from already-read bytes (one file read for hash + metadata + thumbnail),
/// tagging it with the original path so extension-based decoder selection works.
pub fn source_from_bytes(bytes: Arc<Vec<u8>>, path: &Path) -> RawSource {
    RawSource::new_from_shared_vec(bytes).with_path(path)
}

/// Open a [`RawSource`] directly from a path.
pub fn source_from_path(path: &Path) -> std::io::Result<RawSource> {
    RawSource::new(path)
}
