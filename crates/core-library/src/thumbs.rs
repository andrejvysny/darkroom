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

    /// Path for an edited thumbnail, keyed by both the renderer's process version and the edit's
    /// `updated_at` revision. The stored edit's process version is provenance; the current renderer
    /// version identifies the pixels and invalidates old renders without rewriting saved edits.
    pub fn edited_path_for(&self, hash_hex: &str, pv: i64, version: i64) -> PathBuf {
        self.root
            .join(format!("{hash_hex}_edit_pv{pv}_{version}.jpg"))
    }

    pub fn read_edited(&self, hash_hex: &str, pv: i64, version: i64) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.edited_path_for(hash_hex, pv, version))
    }

    /// Write an edited thumbnail atomically. Keep other revisions until ordinary cache eviction:
    /// an older render can finish late and must not remove a newer render or another process version.
    pub fn write_edited(
        &self,
        hash_hex: &str,
        pv: i64,
        version: i64,
        jpeg: &[u8],
    ) -> std::io::Result<()> {
        let final_path = self.edited_path_for(hash_hex, pv, version);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(
            "{hash_hex}_edit_pv{pv}_{version}.{}.{seq}.tmp",
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

    /// Path for the *canonical* develop render: `‹hash›_dev‹pv›.jpg`, where `pv` is the pipeline
    /// `process_version`. This is the unified-render thumbnail for an UNEDITED image (the GPU pipeline
    /// at default params), the single source of truth that the grid/filmstrip/loupe/develop all show.
    /// Keyed by `pv` so a pipeline-default change auto-invalidates every canonical thumb.
    pub fn canonical_path_for(&self, hash_hex: &str, pv: i64) -> PathBuf {
        self.root.join(format!("{hash_hex}_dev{pv}.jpg"))
    }

    pub fn has_canonical(&self, hash_hex: &str, pv: i64) -> bool {
        self.canonical_path_for(hash_hex, pv).exists()
    }

    pub fn read_canonical(&self, hash_hex: &str, pv: i64) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.canonical_path_for(hash_hex, pv))
    }

    /// Write the canonical render for `pv`, then sweep any stale `‹hash›_dev*` variants (a prior
    /// process version) so only the current one survives. Publish-then-sweep: a failed write keeps
    /// the old file.
    pub fn write_canonical(&self, hash_hex: &str, pv: i64, jpeg: &[u8]) -> std::io::Result<()> {
        self.publish(
            &format!("{hash_hex}_dev{pv}.jpg"),
            jpeg,
            &[format!("{hash_hex}_dev")],
        )
    }

    /// Remove every canonical variant (`‹hash›_dev*.jpg`) for a hash. Returns count removed.
    pub fn clear_canonical(&self, hash_hex: &str) -> std::io::Result<usize> {
        let prefix = format!("{hash_hex}_dev");
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

    // ── Display-sharp preview tier (loupe / develop first-paint) ───────────────────────────────
    // Larger than the `_dev` thumb (configurable `preview_edge`), keyed by edge so a setting change
    // re-renders cleanly. Evictable (LRU-capped) — unlike the durable `_dev` thumb — since a full
    // library of previews is large. Canonical (default-params) and edited variants are separate.

    /// Path for the canonical (default-params) preview: `‹hash›_pv‹pv›_‹edge›.jpg`.
    pub fn preview_path_for(&self, hash_hex: &str, pv: i64, edge: u32) -> PathBuf {
        self.root.join(format!("{hash_hex}_pv{pv}_{edge}.jpg"))
    }

    pub fn has_preview(&self, hash_hex: &str, pv: i64, edge: u32) -> bool {
        self.preview_path_for(hash_hex, pv, edge).exists()
    }

    pub fn read_preview(&self, hash_hex: &str, pv: i64, edge: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.preview_path_for(hash_hex, pv, edge))
    }

    /// Write the canonical preview, then sweep stale `‹hash›_pv*` variants (prior pv/edge) and every
    /// `‹hash›_emb_*` (a rendered preview supersedes the embedded one). Publish-then-sweep: a failed
    /// write keeps the old files.
    pub fn write_preview(
        &self,
        hash_hex: &str,
        pv: i64,
        edge: u32,
        jpeg: &[u8],
    ) -> std::io::Result<()> {
        self.publish(
            &format!("{hash_hex}_pv{pv}_{edge}.jpg"),
            jpeg,
            &[format!("{hash_hex}_pv"), format!("{hash_hex}_emb_")],
        )
    }

    // ── Embedded-derived preview tier (written at import from the camera's embedded JPEG) ──────

    /// Path for the embedded-derived preview: `‹hash›_emb_‹edge›.jpg`.
    pub fn embedded_preview_path_for(&self, hash_hex: &str, edge: u32) -> PathBuf {
        self.root.join(format!("{hash_hex}_emb_{edge}.jpg"))
    }

    pub fn has_embedded_preview(&self, hash_hex: &str, edge: u32) -> bool {
        self.embedded_preview_path_for(hash_hex, edge).exists()
    }

    pub fn read_embedded_preview(&self, hash_hex: &str, edge: u32) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.embedded_preview_path_for(hash_hex, edge))
    }

    /// Write the embedded-derived preview, then sweep other `‹hash›_emb_*` edges.
    pub fn write_embedded_preview(
        &self,
        hash_hex: &str,
        edge: u32,
        jpeg: &[u8],
    ) -> std::io::Result<()> {
        self.publish(
            &format!("{hash_hex}_emb_{edge}.jpg"),
            jpeg,
            &[format!("{hash_hex}_emb_")],
        )
    }

    /// Atomically publish `final_name` (unique temp + rename), and only AFTER that succeeds remove
    /// every other `.jpg` starting with one of `sweep_prefixes`. Sweeping first would lose the old
    /// file when the write fails.
    fn publish(
        &self,
        final_name: &str,
        jpeg: &[u8],
        sweep_prefixes: &[String],
    ) -> std::io::Result<()> {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .root
            .join(format!("{final_name}.{}.{seq}.tmp", std::process::id()));
        std::fs::write(&tmp, jpeg)?;
        if let Err(e) = std::fs::rename(&tmp, self.root.join(final_name)) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        for prefix in sweep_prefixes {
            let _ = self.clear_prefix_except(prefix, final_name);
        }
        Ok(())
    }

    /// Path for an edited preview, keyed by renderer process version, edit revision and size.
    pub fn edited_preview_path_for(
        &self,
        hash_hex: &str,
        pv: i64,
        version: i64,
        edge: u32,
    ) -> PathBuf {
        self.root
            .join(format!("{hash_hex}_editpv_pv{pv}_{version}_{edge}.jpg"))
    }

    pub fn has_edited_preview(&self, hash_hex: &str, pv: i64, version: i64, edge: u32) -> bool {
        self.edited_preview_path_for(hash_hex, pv, version, edge)
            .exists()
    }

    pub fn read_edited_preview(
        &self,
        hash_hex: &str,
        pv: i64,
        version: i64,
        edge: u32,
    ) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.edited_preview_path_for(hash_hex, pv, version, edge))
    }

    /// Write an edited preview without deleting other revisions that may have completed earlier.
    /// All edited variants remain subject to the ordinary bounded cache eviction policy.
    pub fn write_edited_preview(
        &self,
        hash_hex: &str,
        pv: i64,
        version: i64,
        edge: u32,
        jpeg: &[u8],
    ) -> std::io::Result<()> {
        let final_path = self.edited_preview_path_for(hash_hex, pv, version, edge);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = self.root.join(format!(
            "{hash_hex}_editpv_pv{pv}_{version}_{edge}.{}.{seq}.tmp",
            std::process::id()
        ));
        std::fs::write(&tmp, jpeg)?;
        std::fs::rename(&tmp, &final_path)
    }

    /// Remove every cached `.jpg` whose name starts with `prefix`, except `keep`.
    fn clear_prefix_except(&self, prefix: &str, keep: &str) -> std::io::Result<usize> {
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name != keep
                && name.starts_with(prefix)
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
    ///
    /// Canonical develop thumbnails (`‹hash›_dev‹pv›.jpg`) are **exempt** — they are the durable
    /// unified-render source of truth, expensive to regenerate (full demosaic + GPU), and evicting
    /// one would resurrect the camera-look placeholder and re-trigger a background render. Camera
    /// placeholders and edited variants evict normally.
    pub fn evict_to(&self, cap_bytes: u64) -> std::io::Result<u64> {
        // (path, last_used, size) for every evictable cached thumbnail.
        let mut files: Vec<(PathBuf, bool, std::time::SystemTime, u64)> = Vec::new();
        let mut total: u64 = 0;
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // `_dev` only ever appears in canonical filenames (the hash is pure hex); skip them so
            // their bytes don't count toward the cap and they are never selected for eviction.
            if !name.ends_with(".jpg") || name.contains("_dev") {
                continue;
            }
            let meta = entry.metadata()?;
            let used = meta
                .accessed()
                .or_else(|_| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            total += meta.len();
            files.push((entry.path(), name.contains("_emb_"), used, meta.len()));
        }
        if total <= cap_bytes {
            return Ok(0);
        }
        // Embedded-derived previews first (cheap to regenerate from the source file's embedded
        // JPEG), then oldest first; delete until under cap.
        files.sort_by_key(|(_, emb, used, _)| (!*emb, *used));
        let mut freed = 0;
        for (path, _, _, size) in files {
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

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCache(ThumbCache);

    impl TestCache {
        fn new() -> Self {
            let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("darkroom-thumb-cache-{}-{seq}", std::process::id()));
            Self(ThumbCache::new(root).unwrap())
        }
    }

    impl Drop for TestCache {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.root());
        }
    }

    #[test]
    fn renderer_change_invalidates_edited_thumbnails_and_previews() {
        let cache = TestCache::new();
        let cache = &cache.0;
        let hash = "ab".repeat(32);
        let revision = 1_700_000_000;
        cache
            .write_edited(&hash, 5, revision, b"old thumb")
            .unwrap();
        cache
            .write_edited_preview(&hash, 5, revision, 1536, b"old preview")
            .unwrap();
        // Legacy unversioned entries must not be accepted either.
        std::fs::write(
            cache.root().join(format!("{hash}_edit_{revision}.jpg")),
            b"legacy",
        )
        .unwrap();
        std::fs::write(
            cache
                .root()
                .join(format!("{hash}_editpv_{revision}_1536.jpg")),
            b"legacy",
        )
        .unwrap();

        assert!(cache.read_edited(&hash, 6, revision).is_err());
        assert!(!cache.has_edited_preview(&hash, 6, revision, 1536));
        assert!(cache.read_edited_preview(&hash, 6, revision, 1536).is_err());

        cache
            .write_edited(&hash, 6, revision, b"new thumb")
            .unwrap();
        cache
            .write_edited_preview(&hash, 6, revision, 1536, b"new preview")
            .unwrap();
        assert_eq!(cache.read_edited(&hash, 6, revision).unwrap(), b"new thumb");
        assert_eq!(
            cache.read_edited_preview(&hash, 6, revision, 1536).unwrap(),
            b"new preview"
        );
        assert!(cache.read_edited(&hash, 6, revision + 1).is_err());
        assert!(!cache.has_edited_preview(&hash, 6, revision, 1024));
    }

    #[test]
    fn late_old_render_cannot_remove_newer_revision_or_renderer() {
        let cache = TestCache::new();
        let cache = &cache.0;
        let hash = "cd".repeat(32);
        cache.write_edited(&hash, 6, 102, b"new thumb").unwrap();
        cache
            .write_edited_preview(&hash, 6, 102, 1536, b"new preview")
            .unwrap();

        for (pv, revision) in [(6, 101), (5, 102)] {
            cache
                .write_edited(&hash, pv, revision, b"late thumb")
                .unwrap();
            cache
                .write_edited_preview(&hash, pv, revision, 1536, b"late preview")
                .unwrap();
        }
        assert_eq!(cache.read_edited(&hash, 6, 102).unwrap(), b"new thumb");
        assert_eq!(
            cache.read_edited_preview(&hash, 6, 102, 1536).unwrap(),
            b"new preview"
        );

        // Retained revisions still obey the cache cap and never become durable canonical entries.
        assert!(cache.evict_to(0).unwrap() > 0);
        assert_eq!(cache.total_size().unwrap(), 0);
    }

    #[test]
    fn preview_publishes_before_sweeping_and_replaces_embedded() {
        let cache = TestCache::new();
        let cache = &cache.0;
        let h = "ef".repeat(32);
        cache.write_embedded_preview(&h, 2560, b"emb").unwrap();
        cache.write_embedded_preview(&h, 3000, b"emb3").unwrap();
        assert!(!cache.has_embedded_preview(&h, 2560), "other edges swept");
        cache.write_preview(&h, 5, 2560, b"old").unwrap();
        assert!(
            !cache.has_embedded_preview(&h, 3000),
            "rendered supersedes emb"
        );
        cache.write_preview(&h, 6, 2560, b"new").unwrap();
        assert!(!cache.has_preview(&h, 5, 2560));
        assert_eq!(cache.read_preview(&h, 6, 2560).unwrap(), b"new");
    }

    #[test]
    fn failed_publish_keeps_old_files() {
        let cache = TestCache::new();
        let cache = &cache.0;
        let h = "12".repeat(32);
        cache.write_preview(&h, 5, 2560, b"old").unwrap();
        cache.write_embedded_preview(&h, 2560, b"emb").unwrap();
        cache.write_canonical(&h, 1, b"dev1").unwrap();
        // A directory occupying the final path makes rename fail.
        std::fs::create_dir(cache.preview_path_for(&h, 6, 2560)).unwrap();
        assert!(cache.write_preview(&h, 6, 2560, b"new").is_err());
        assert_eq!(cache.read_preview(&h, 5, 2560).unwrap(), b"old");
        assert!(cache.has_embedded_preview(&h, 2560), "emb kept on failure");
        std::fs::create_dir(cache.canonical_path_for(&h, 2)).unwrap();
        assert!(cache.write_canonical(&h, 2, b"dev2").is_err());
        assert_eq!(cache.read_canonical(&h, 1).unwrap(), b"dev1");
        std::fs::create_dir(cache.embedded_preview_path_for(&h, 3000)).unwrap();
        assert!(cache.write_embedded_preview(&h, 3000, b"x").is_err());
        assert!(cache.has_embedded_preview(&h, 2560));
        // No stray temp files left behind.
        let tmps = std::fs::read_dir(cache.root())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(tmps, 0);
    }

    #[test]
    fn eviction_takes_embedded_first_and_never_dev() {
        let cache = TestCache::new();
        let cache = &cache.0;
        let h = "34".repeat(32);
        // Written in this order so the embedded file is the NEWEST by mtime/atime.
        cache.write(&h, 512, &[0u8; 100]).unwrap();
        cache.write_canonical(&h, 1, &[0u8; 100]).unwrap();
        cache.write_embedded_preview(&h, 2560, &[0u8; 100]).unwrap();
        // Cap leaves room for exactly one evictable file: only the emb file goes.
        assert_eq!(cache.evict_to(100).unwrap(), 100);
        assert!(!cache.has_embedded_preview(&h, 2560));
        assert!(cache.has(&h, 512));
        assert!(cache.has_canonical(&h, 1));
        cache.evict_to(0).unwrap();
        assert!(!cache.has(&h, 512));
        assert!(cache.has_canonical(&h, 1), "_dev is exempt");
    }
}
