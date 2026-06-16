//! On-disk thumbnail cache keyed by `‹content_hash_hex›_‹size›.jpg`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Per-process counter so concurrent writers (incl. byte-identical dupes) use distinct temp files.
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
pub struct ThumbCache {
    root: PathBuf,
}

impl ThumbCache {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, hash_hex: &str, size: u32) -> PathBuf {
        self.root.join(format!("{hash_hex}_{size}.jpg"))
    }

    pub fn has(&self, hash_hex: &str, size: u32) -> bool {
        self.path_for(hash_hex, size).exists()
    }

    pub fn read(&self, hash_hex: &str, size: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.path_for(hash_hex, size))
    }

    /// Atomic write (unique temp file + rename) so readers never see a partial JPEG and
    /// concurrent writers (incl. byte-identical dupes hashing to the same name) never collide.
    pub fn write(&self, hash_hex: &str, size: u32, jpeg: &[u8]) -> std::io::Result<()> {
        let final_path = self.path_for(hash_hex, size);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(
            "{hash_hex}_{size}.{}.{seq}.tmp",
            std::process::id()
        ));
        std::fs::write(&tmp, jpeg)?;
        std::fs::rename(&tmp, &final_path)
    }
}
