//! core-import — ingest RAW files from a source (e.g. an SD card) into the library.
//!
//! Modes: copy+add, move+add (verified before source deletion), reference (add-in-place).
//! Copy/move route into `‹library_root›/YYYY/YYYY-MM-DD/` by EXIF capture date, verify the
//! destination by content hash, handle filename collisions, and skip already-catalogued files.

pub mod error;

pub use error::ImportError;

use chrono::DateTime;
use core_db::rusqlite::{params, Connection};
use core_library::{insert_image, now_epoch, process_file, ThumbCache, THUMB_SIZE};
use core_raw::{hash_file, read_metadata, source_from_path};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    Copy,
    Move,
    Reference,
}

impl ImportMode {
    fn as_str(self) -> &'static str {
        match self {
            ImportMode::Copy => "copy",
            ImportMode::Move => "move",
            ImportMode::Reference => "reference",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportStats {
    pub session_id: i64,
    pub total: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
}

/// `YYYY/YYYY-MM-DD` from an epoch-seconds capture date (naive-as-UTC, matching how it was stored).
fn date_subpath(epoch: i64) -> String {
    DateTime::from_timestamp(epoch, 0)
        .map(|dt| dt.format("%Y/%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown/unknown".to_string())
}

fn file_mtime_epoch(path: &Path) -> i64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pick a non-colliding destination path within `dir` for `filename` (suffixes `_1`, `_2`, …).
fn unique_dest(dir: &Path, filename: &str) -> PathBuf {
    let primary = dir.join(filename);
    if !primary.exists() {
        return primary;
    }
    let path = Path::new(filename);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 1.. {
        let name = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
        let cand = dir.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    unreachable!()
}

fn create_session(conn: &Connection, source: &str, mode: ImportMode) -> Result<i64, ImportError> {
    conn.execute(
        "INSERT INTO import_sessions(source_volume, mode, started_at) VALUES(?1, ?2, ?3)",
        params![source, mode.as_str(), now_epoch()],
    )?;
    Ok(conn.last_insert_rowid())
}

fn finish_session(conn: &Connection, stats: &ImportStats) -> Result<(), ImportError> {
    conn.execute(
        "UPDATE import_sessions SET finished_at=?1, file_count=?2, skipped_count=?3 WHERE id=?4",
        params![
            now_epoch(),
            stats.added as i64,
            stats.skipped as i64,
            stats.session_id
        ],
    )?;
    Ok(())
}

/// Run an import. `progress(done, total)` fires per file.
pub fn import<F>(
    conn: &mut Connection,
    thumbs: &ThumbCache,
    source: &Path,
    mode: ImportMode,
    library_root: &Path,
    progress: F,
) -> Result<ImportStats, ImportError>
where
    F: Fn(usize, usize),
{
    let files = core_library::enumerate_raws(source);
    let total = files.len();

    // Destination folder row (copy/move target = library root; reference = the source itself).
    let folder_id = match mode {
        ImportMode::Reference => core_library::add_root(conn, source)?,
        _ => core_library::add_root(conn, library_root)?,
    };

    // Preload existing content hashes to skip already-catalogued files.
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT content_hash FROM images")?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
        for h in rows.flatten() {
            if h.len() == 32 {
                let mut a = [0u8; 32];
                a.copy_from_slice(&h);
                seen.insert(a);
            }
        }
    }

    let session_id = create_session(conn, &source.display().to_string(), mode)?;
    let mut stats = ImportStats {
        session_id,
        total,
        ..Default::default()
    };
    let imported_at = now_epoch();

    for (i, src_path) in files.iter().enumerate() {
        match import_one(
            conn,
            thumbs,
            src_path,
            mode,
            library_root,
            folder_id,
            session_id,
            imported_at,
            &mut seen,
        ) {
            Ok(true) => stats.added += 1,
            Ok(false) => stats.skipped += 1,
            Err(_) => stats.failed += 1,
        }
        progress(i + 1, total);
    }

    finish_session(conn, &stats)?;
    Ok(stats)
}

#[allow(clippy::too_many_arguments)]
fn import_one(
    conn: &Connection,
    thumbs: &ThumbCache,
    src_path: &Path,
    mode: ImportMode,
    library_root: &Path,
    folder_id: i64,
    session_id: i64,
    imported_at: i64,
    seen: &mut HashSet<[u8; 32]>,
) -> Result<bool, ImportError> {
    let (src_hash, _size) = hash_file(src_path)?;
    if seen.contains(&src_hash) {
        return Ok(false); // already in library (or imported this run)
    }

    let dest_path = match mode {
        ImportMode::Reference => src_path.to_path_buf(),
        ImportMode::Copy | ImportMode::Move => {
            // Resolve date folder.
            let src = source_from_path(src_path)?;
            let capture = read_metadata(&src)
                .ok()
                .and_then(|m| m.capture_date)
                .unwrap_or_else(|| file_mtime_epoch(src_path));
            let dest_dir = library_root.join(date_subpath(capture));
            std::fs::create_dir_all(&dest_dir)?;
            let filename = src_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("file.raw");

            let primary = dest_dir.join(filename);
            if primary.exists() {
                let (dh, _) = hash_file(&primary)?;
                if dh == src_hash {
                    // Identical file already at destination — nothing to do.
                    seen.insert(src_hash);
                    return Ok(false);
                }
            }
            let dest = unique_dest(&dest_dir, filename);

            std::fs::copy(src_path, &dest)?;
            let (vh, _) = hash_file(&dest)?;
            if vh != src_hash {
                // Verification failed — remove the bad copy, preserve the source, report failure.
                let _ = trash::delete(&dest);
                return Err(ImportError::Io(std::io::Error::other(
                    "destination hash mismatch after copy",
                )));
            }
            if matches!(mode, ImportMode::Move) {
                // Delete source only AFTER a verified copy.
                trash::delete(src_path)?;
            }
            dest
        }
    };

    let processed = process_file(&dest_path, thumbs, THUMB_SIZE)?;
    if let Some(id) = insert_image(conn, folder_id, imported_at, &processed)? {
        conn.execute(
            "UPDATE images SET import_session_id=?1 WHERE id=?2",
            params![session_id, id],
        )?;
        seen.insert(src_hash);
        Ok(true)
    } else {
        Ok(false)
    }
}
