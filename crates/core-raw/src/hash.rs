//! Byte-identity (content) hashing — BLAKE3 over the whole file.

use std::path::Path;

/// BLAKE3 digest of an in-memory byte buffer.
pub fn content_hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Read a file fully, returning `(content_hash, file_size)`.
pub fn hash_file(path: &Path) -> std::io::Result<([u8; 32], u64)> {
    let bytes = std::fs::read(path)?;
    let size = bytes.len() as u64;
    Ok((content_hash(&bytes), size))
}

/// Lowercase hex of a 32-byte digest (used as cache key / URL component).
pub fn hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
