//! core-raw — RAW decode, embedded preview/thumbnail extraction, metadata, and identity hashing.
//!
//! All `rawler` calls are isolated in this crate (rawler's API is non-SemVer; pinned `=0.8.0`).
//!
//! rawler decodes on a best-effort basis and reaches `todo!()`/`unimplemented!()` on formats and
//! sensor layouts it does not handle. Two layers keep that from taking the app down: the
//! pre-screens in [`develop::guard_developable`] turn the known cases into typed
//! [`error::RawError::Unsupported`] before a decode starts, and [`panic`] catches whatever still
//! panics so one bad file is skipped instead of aborting the process.

pub mod color;
pub mod develop;
pub mod display;
pub mod error;
pub mod hash;
pub mod hdr_dng;
pub mod hdr_file;
pub mod heif;
pub mod meta;
pub mod panic;
pub mod pano;
pub mod thumb;

#[doc(hidden)]
pub mod synth;

pub use color::HDR_DIFFUSE_WHITE_NITS;
pub use develop::{
    as_shot_wb, develop_linear, develop_linear_denoised, develop_linear_preview, develop_linear_wb,
    DenoiseOutput, LinearImage, MosaicDenoiser, MosaicInfo,
};
pub use display::{classify, is_display, ImageKind, RAW_EXT};
pub use error::{FailureKind, RawError};
pub use hash::{content_hash, hash_file, hex};
pub use hdr_dng::write_hdr_dng;
pub use hdr_file::{read_hdr_sources, write_hdr_exr, HdrSourceInfo, HdrSources};
pub use meta::{capture_fingerprint, read_exposure_numeric, read_metadata, RawMeta};
pub use panic::{decode_in_flight, ISOLATION_ACTIVE};
pub use pano::{
    develop_camera_native, native_to_srgb_jpeg, write_pano_dng, CameraNativeImage, PanoColorMeta,
};
pub use thumb::{oriented_preview, preview_image, preview_with_orientation, thumbnail_jpeg, Thumb};

pub use rawler::rawsource::RawSource;

/// Identity of the RAW decoder this build links. Persisted next to every recorded decode failure
/// so a decoder upgrade automatically re-tries files that were "unsupported" under the old one.
/// Keep in sync with the `rawler` pin in `Cargo.toml`.
pub const DECODER_VERSION: &str = "rawler-0.8.0";

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
