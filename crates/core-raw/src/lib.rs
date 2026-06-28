//! core-raw — RAW decode, embedded preview/thumbnail extraction, metadata, and identity hashing.
//!
//! All `rawler` calls are isolated in this crate (rawler's API is non-SemVer; pinned `=0.7.2`).

pub mod develop;
pub mod display;
pub mod error;
pub mod hash;
pub mod meta;
pub mod thumb;

pub use develop::{as_shot_wb, develop_linear, develop_linear_preview, LinearImage};
pub use display::{classify, is_display, ImageKind};
pub use error::RawError;
pub use hash::{content_hash, hash_file, hex};
pub use meta::{capture_fingerprint, read_metadata, RawMeta};
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
