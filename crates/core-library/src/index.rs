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

/// Extensions indexed: RAW — every Canon (`cr3 cr2 crw`), Nikon (`nef nrw`) and Sony (`arw sr2
/// srf`) container rawler decodes, plus DNG — + display-referred JPEG/PNG (decoded via the `image`
/// crate in `core-raw::display`) + scene-referred HDR: Canon 10-bit PQ HEIF (`hif`, via libheif in
/// `core-raw::heif`) and merged-HDR OpenEXR (`exr`, `core-raw::hdr_file`). Single source of truth
/// for indexing, folder scan/watch, and import listing. Other makers (RAF/ORF/RW2/PEF) are
/// deliberately absent until validated against the RAW corpus (X-Trans needs its own demosaic).
pub const SUPPORTED_EXT: &[&str] = &[
    "cr3", "cr2", "crw", "nef", "nrw", "arw", "sr2", "srf", "dng", "jpg", "jpeg", "png", "hif",
    "exr",
];

/// Catalog format bucket for a path (`"raw" | "jpeg" | "png" | "heif" | "hdr"`), via
/// `core_raw::classify`. The single extension→kind classifier used by the `images.format` column
/// and the by-type filters.
pub fn image_kind(path: &Path) -> &'static str {
    core_raw::classify(path).as_str()
}

/// Default grid thumbnail longest-edge (px). 2× headroom for HiDPI cells.
pub const THUMB_SIZE: u32 = 512;

#[derive(Debug, Clone, Default, Serialize)]
pub struct IndexStats {
    pub scanned: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Files this build can never decode (unknown body, unsupported compression). A strict subset
    /// of `failed`, split out so the UI can say "3 unsupported" instead of the useless "3 failed".
    pub unsupported: usize,
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
    /// Catalog format bucket (`"raw" | "jpeg" | "png"`), from the file extension.
    pub format: &'static str,
}

pub fn now_epoch() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn is_supported(path: &Path) -> bool {
    // Hidden files are never images: Finder writes `._IMG_0001.CR3` AppleDouble shadows onto
    // exFAT cards, and they carry the RAW's extension while holding only metadata bytes.
    let hidden = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.starts_with('.'))
        .unwrap_or(true);
    if hidden {
        return false;
    }
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

/// File modification time as epoch seconds, or `None` if unavailable. Used as the capture-date
/// fallback when EXIF carries no `DateTimeOriginal` (mirrors core-import's path-routing fallback).
fn file_mtime_epoch(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
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
    process_bytes(path, bytes, digest, thumbs, thumb_size)
}

/// Everything [`process_file`] does *after* the read + hash, over a buffer the caller already holds.
///
/// The importer reads each source exactly once and then needs the same metadata/thumbnail pass over
/// those bytes; going back through `process_file` would re-read (and re-hash) the whole file a
/// second time — a full extra pass over every 30 MB RAW in an import. `path` is the CATALOG path
/// (the library copy for copy/move, the source itself for reference), which need not be where
/// `bytes` were read from; `digest` must be `content_hash(&bytes)` and `file_size` is derived from
/// the buffer, so both stay consistent with the bytes actually processed.
pub fn process_bytes(
    path: &Path,
    bytes: Arc<Vec<u8>>,
    digest: [u8; 32],
    thumbs: &ThumbCache,
    thumb_size: u32,
) -> Result<ProcessedImage, LibError> {
    let hex_digest = hex(&digest);
    let file_size = bytes.len() as i64;

    let src = source_from_bytes(bytes, path);
    let mut meta = read_metadata(&src)?;
    // Never store a NULL capture_date: when EXIF has no DateTimeOriginal, fall back to the file's
    // mtime — the same value the importer uses to route the file into its `YYYY/YYYY-MM-DD` folder
    // (core-import `date_subpath`). This keeps capture-date sort, the Folders tree, and the on-disk
    // layout consistent (a no-EXIF file no longer sorts/groups as "Unknown" while living in a dated
    // folder). Legacy rows imported before this stay NULL (no backfill) and fall into the NULL block.
    if meta.capture_date.is_none() {
        meta.capture_date = file_mtime_epoch(path);
    }
    let thumb = core_raw::thumbnail_jpeg(&src, thumb_size, 82)?;
    thumbs.write(&hex_digest, thumb_size, &thumb.jpeg)?;

    // Fingerprint keys off NATIVE (pre-orientation) dims for stability; the catalog stores the
    // ORIENTED display dims so portrait shots aren't recorded as landscape (correct aspect/UI).
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
        width: thumb.disp_width as i64,
        height: thumb.disp_height as i64,
        capture_fingerprint: fp,
        format: image_kind(path),
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
            aperture, focal_length, width, height, orientation, exif, imported_at, format
         ) VALUES (?1,?2,?3,?4,?5,?6,'present',?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
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
            p.format,
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
    // Files whose recorded decode failure still stands (same decoder build, same bytes) are not
    // re-opened: without this a card of unknown-body RAWs is fully re-decoded on every scan.
    let known_bad = crate::decode_failure::skippable_failures(conn)?;
    let mut skipped_bad = 0usize;
    let todo: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| !known.contains(&p.display().to_string()))
        .filter(|p| {
            let bad = known_bad.contains(&p.display().to_string());
            skipped_bad += usize::from(bad);
            !bad
        })
        .collect();

    let total = todo.len();
    let done = AtomicUsize::new(0);
    // The path rides along with its result: a `LibError` carries no path, and every failure has to
    // be recorded against the file that produced it.
    let results: Vec<(&PathBuf, Result<ProcessedImage, LibError>)> = todo
        .par_iter()
        .map(|p| {
            let r = process_file(p, thumbs, thumb_size);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(n, total);
            (p, r)
        })
        .collect();

    let imported_at = now_epoch();
    let mut stats = IndexStats {
        scanned: total,
        skipped: skipped_bad,
        ..Default::default()
    };
    let tx = conn.transaction()?;
    for (path, r) in &results {
        match r {
            Ok(p) => match insert_image(&tx, folder_id, imported_at, p)? {
                Some(id) => {
                    stats.added += 1;
                    // Recover edits/rating/keywords from a sidecar next to the RAW (e.g. after a
                    // "delete catalog.db, rescan" rebuild). Only blank rows hydrate; best-effort.
                    let _ = crate::sidecar::hydrate_if_blank(&tx, id, &p.path);
                    // A file that used to fail and now indexes leaves no stale record behind.
                    crate::decode_failure::clear_decode_failure(&tx, &p.path)?;
                }
                None => stats.skipped += 1,
            },
            Err(e) => {
                stats.failed += 1;
                // Recording is best-effort: a bookkeeping write must never fail a whole scan.
                if crate::decode_failure::record_decode_failure(
                    &tx,
                    Some(folder_id),
                    path,
                    e,
                    imported_at,
                )
                .unwrap_or(false)
                {
                    stats.unsupported += 1;
                }
            }
        }
    }
    tx.commit()?;
    Ok(stats)
}

#[cfg(test)]
mod supported_tests {
    use super::*;

    #[test]
    fn maker_extensions_are_supported_case_insensitively() {
        for ext in [
            "cr3", "CR2", "crw", "NEF", "nrw", "arw", "SR2", "srf", "dng", "HIF", "exr",
        ] {
            assert!(is_supported(Path::new(&format!("IMG_0001.{ext}"))), "{ext}");
        }
        assert!(!is_supported(Path::new("IMG_0001.raf")));
        assert!(!is_supported(Path::new("IMG_0001.xmp")));
        assert!(!is_supported(Path::new("IMG_0001")));
    }

    /// The allowlist and core-raw's classifier must evolve together: every RAW extension core-raw
    /// expects to see must be indexable, and every indexable extension must land in a known kind.
    #[test]
    fn allowlist_matches_core_raw_classifier() {
        for ext in core_raw::RAW_EXT {
            assert!(
                SUPPORTED_EXT.contains(ext),
                "core-raw RAW_EXT `{ext}` missing from SUPPORTED_EXT"
            );
        }
        for ext in SUPPORTED_EXT {
            let kind = core_raw::classify(Path::new(&format!("x.{ext}")));
            let known = core_raw::RAW_EXT.contains(ext) || kind != core_raw::ImageKind::Raw;
            assert!(
                known,
                "SUPPORTED_EXT `{ext}` is neither a known RAW ext nor a display/HDR kind"
            );
        }
    }

    #[test]
    fn hidden_and_appledouble_files_are_skipped() {
        assert!(!is_supported(Path::new("._IMG_0001.CR3")));
        assert!(!is_supported(Path::new(".IMG_0001.CR3")));
        assert!(!is_supported(Path::new("/cards/DCIM/._DSC0001.ARW")));
    }
}
