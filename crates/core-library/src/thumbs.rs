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

    /// Path for an *edited* thumbnail variant: `‹hash›_edit_‹version›.jpg`, where `version` is the
    /// edit's `updated_at`. Kept separate from the base (unedited) thumbnail so toggling/resetting an
    /// edit never destroys the original, and so the URL changes when the edit does (cache-busting).
    pub fn edited_path_for(&self, hash_hex: &str, version: i64) -> PathBuf {
        self.root.join(format!("{hash_hex}_edit_{version}.jpg"))
    }

    pub fn read_edited(&self, hash_hex: &str, version: i64) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.edited_path_for(hash_hex, version))
    }

    /// Write the edited thumbnail for `version`, first removing any older edited variants of this
    /// hash (only the current version is ever requested, so stale ones are dead weight).
    pub fn write_edited(&self, hash_hex: &str, version: i64, jpeg: &[u8]) -> std::io::Result<()> {
        let _ = self.clear_edited(hash_hex);
        let final_path = self.edited_path_for(hash_hex, version);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(
            "{hash_hex}_edit_{version}.{}.{seq}.tmp",
            std::process::id()
        ));
        std::fs::write(&tmp, jpeg)?;
        std::fs::rename(&tmp, &final_path)
    }

    /// Remove every edited variant (`‹hash›_edit_*.jpg`) for a hash. Returns count removed.
    pub fn clear_edited(&self, hash_hex: &str) -> std::io::Result<usize> {
        let prefix = format!("{hash_hex}_edit_");
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&prefix)
                && name.ends_with(".jpg")
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Delete every cached size variant for a content hash (`‹hash›_*.jpg`). Call when an image row
    /// is removed (dedup resolve, import move) and no other present row shares the hash. Returns the
    /// number of files deleted. Missing files are not an error.
    pub fn remove_hash(&self, hash_hex: &str) -> std::io::Result<usize> {
        let prefix = format!("{hash_hex}_");
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&prefix)
                && name.ends_with(".jpg")
                && std::fs::remove_file(entry.path()).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Total bytes of cached `.jpg` thumbnails (ignores in-flight `.tmp` files).
    pub fn total_size(&self) -> std::io::Result<u64> {
        let mut total = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let is_jpg = name.to_str().is_some_and(|n| n.ends_with(".jpg"));
            if is_jpg {
                total += entry.metadata()?.len();
            }
        }
        Ok(total)
    }

    /// Evict least-recently-used thumbnails until the cache is at or under `cap_bytes`. "Recently
    /// used" is the file's access time, falling back to modified time (atime is unreliable on some
    /// mounts). Returns bytes freed. A no-op when already under the cap or the dir is missing.
    pub fn evict_to(&self, cap_bytes: u64) -> std::io::Result<u64> {
        // (path, last_used, size) for every cached thumbnail.
        let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
        let mut total: u64 = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            if !name.to_str().is_some_and(|n| n.ends_with(".jpg")) {
                continue;
            }
            let meta = entry.metadata()?;
            let used = meta
                .accessed()
                .or_else(|_| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            total += meta.len();
            files.push((entry.path(), used, meta.len()));
        }
        if total <= cap_bytes {
            return Ok(0);
        }
        // Oldest first; delete until under cap.
        files.sort_by_key(|(_, used, _)| *used);
        let mut freed = 0;
        for (path, _, size) in files {
            if total <= cap_bytes {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= size;
                freed += size;
            }
        }
        Ok(freed)
    }
}
