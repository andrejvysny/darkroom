//! Folder indexing: enumerate RAW files, hash + extract metadata + generate thumbnails (parallel),
//! then insert catalog rows. Designed so the app holds the DB lock only briefly:
//! enumerate → (unlocked, parallel) `process_file` → (locked) `insert_image`.

use crate::error::LibError;
use crate::thumbs::ThumbCache;
use core_db::rusqlite::{params, Connection, OptionalExtension};
use core_raw::{capture_fingerprint, content_hash, hex, read_metadata, source_from_bytes, RawMeta};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use walkdir::WalkDir;

/// RAW extensions indexed in v1 (CR3 validated; others latent via rawler).
pub const SUPPORTED_EXT: &[&str] = &["cr3", "cr2", "arw", "nef", "dng"];

/// Default grid thumbnail longest-edge (px). 2× headroom for HiDPI cells.
pub const THUMB_SIZE: u32 = 512;

#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStats {
    pub scanned: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// Fully processed (decoded/hashed) image, ready for DB insertion. No DB access required to build.
pub struct ProcessedImage {
    pub content_hash: [u8; 32],
    pub content_hash_hex: String,
    pub file_size: i64,
    pub path: String,
    pub original_filename: String,
    pub meta: RawMeta,
    pub width: i64,
    pub height: i64,
    pub capture_fingerprint: Option<[u8; 32]>,
}

pub fn now_epoch() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|s| SUPPORTED_EXT.iter().any(|e| s.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// List supported RAW files under `root`. When `recursive` is false, only the top-level directory
/// is scanned (subfolders are ignored); when true, the whole tree is walked.
pub fn enumerate_raws(root: &Path, recursive: bool) -> Vec<PathBuf> {
    WalkDir::new(root)
        .max_depth(if recursive { usize::MAX } else { 1 })
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| is_supported(p))
        .collect()
}

/// Insert (or fetch) a watched-folder row; returns its id.
pub fn add_root(conn: &Connection, path: &Path) -> Result<i64, LibError> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let p = canonical.display().to_string();
    conn.execute(
        "INSERT INTO folders(path, is_watched, added_at) VALUES(?1, 1, ?2)
         ON CONFLICT(path) DO NOTHING",
        params![p, now_epoch()],
    )?;
    let id = conn.query_row("SELECT id FROM folders WHERE path=?1", params![p], |r| {
        r.get(0)
    })?;
    Ok(id)
}

/// Set of image paths already in the catalog (for cheap rescan skip-by-path).
pub fn existing_paths(conn: &Connection) -> Result<std::collections::HashSet<String>, LibError> {
    let mut stmt = conn.prepare("SELECT path FROM images")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Hash + metadata + thumbnail for one file (no DB access; safe to run in parallel).
/// Writes the thumbnail to `thumbs` as a side effect.
pub fn process_file(
    path: &Path,
    thumbs: &ThumbCache,
    thumb_size: u32,
) -> Result<ProcessedImage, LibError> {
    let bytes = Arc::new(std::fs::read(path)?);
    let digest = content_hash(&bytes);
    let hex_digest = hex(&digest);
    let file_size = bytes.len() as i64;

    let src = source_from_bytes(bytes, path);
    let meta = read_metadata(&src)?;
    let thumb = core_raw::thumbnail_jpeg(&src, thumb_size, 82)?;
    thumbs.write(&hex_digest, thumb_size, &thumb.jpeg)?;

    let fp = capture_fingerprint(&meta, thumb.src_width, thumb.src_height);

    Ok(ProcessedImage {
        content_hash: digest,
        content_hash_hex: hex_digest,
        file_size,
        path: path.display().to_string(),
        original_filename: path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default(),
        meta,
        width: thumb.src_width as i64,
        height: thumb.src_height as i64,
        capture_fingerprint: fp,
    })
}

/// Insert one processed image. Returns `Ok(Some(id))` if inserted, `Ok(None)` if a byte-identical
/// duplicate (same `content_hash`) is already catalogued.
pub fn insert_image(
    conn: &Connection,
    folder_id: i64,
    imported_at: i64,
    p: &ProcessedImage,
) -> Result<Option<i64>, LibError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT id FROM images WHERE content_hash = ?1",
            params![&p.content_hash[..]],
            |r| r.get(0),
        )
        .optional()?;
    if exists.is_some() {
        return Ok(None);
    }

    let exif_blob = serde_json::to_vec(&p.meta)?;
    let fp_slice: Option<&[u8]> = p.capture_fingerprint.as_ref().map(|f| &f[..]);

    conn.execute(
        "INSERT INTO images(
            content_hash, capture_fingerprint, file_size, path, folder_id, original_filename,
            status, capture_date, camera_make, camera_model, body_serial, lens, iso, shutter,
            aperture, focal_length, width, height, orientation, exif, imported_at
         ) VALUES (?1,?2,?3,?4,?5,?6,'present',?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        params![
            &p.content_hash[..],
            fp_slice,
            p.file_size,
            p.path,
            folder_id,
            p.original_filename,
            p.meta.capture_date,
            p.meta.camera_make,
            p.meta.camera_model,
            p.meta.body_serial,
            p.meta.lens,
            p.meta.iso,
            p.meta.shutter,
            p.meta.aperture,
            p.meta.focal_length,
            p.width,
            p.height,
            p.meta.orientation,
            exif_blob,
            imported_at,
        ],
    )?;
    Ok(Some(conn.last_insert_rowid()))
}

/// Recover a deleted-then-re-imported file. If a row with this `content_hash` exists but is
/// `status='missing'` (its on-disk original was removed, so `reconcile` flagged it), repoint that row
/// to the freshly-imported copy and mark it present — keeping the original image id so any
/// edits/keywords/collections stay attached. Returns the relinked id, or `None` when no missing row
/// matches (the caller then inserts a fresh row). A still-`present` duplicate is left untouched.
pub fn relink_missing_image(
    conn: &Connection,
    folder_id: i64,
    imported_at: i64,
    p: &ProcessedImage,
) -> Result<Option<i64>, LibError> {
    let missing: Option<i64> = conn
        .query_row(
            "SELECT id FROM images WHERE content_hash = ?1 AND status = 'missing'",
            params![&p.content_hash[..]],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = missing else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE images SET path = ?1, folder_id = ?2, status = 'present', imported_at = ?3
         WHERE id = ?4",
        params![p.path, folder_id, imported_at, id],
    )?;
    Ok(Some(id))
}

/// End-to-end scan of a folder: enumerate → parallel process → transactional insert.
/// `progress(done, total)` is invoked as each file finishes processing.
pub fn scan_root<F>(
    conn: &mut Connection,
    thumbs: &ThumbCache,
    folder_id: i64,
    root: &Path,
    thumb_size: u32,
    progress: F,
) -> Result<IndexStats, LibError>
where
    F: Fn(usize, usize) + Sync + Send,
{
    let all = enumerate_raws(root, true);
    let known = existing_paths(conn)?;
    let todo: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| !known.contains(&p.display().to_string()))
        .collect();

    let total = todo.len();
    let done = AtomicUsize::new(0);
    let results: Vec<Result<ProcessedImage, LibError>> = todo
        .par_iter()
        .map(|p| {
            let r = process_file(p, thumbs, thumb_size);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(n, total);
            r
        })
        .collect();

    let imported_at = now_epoch();
    let mut stats = IndexStats {
        scanned: total,
        ..Default::default()
    };
    let tx = conn.transaction()?;
    for r in &results {
        match r {
            Ok(p) => match insert_image(&tx, folder_id, imported_at, p)? {
                Some(_) => stats.added += 1,
                None => stats.skipped += 1,
            },
            Err(_) => stats.failed += 1,
        }
    }
    tx.commit()?;
    Ok(stats)
}
