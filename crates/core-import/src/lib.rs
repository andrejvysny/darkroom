//! core-import — ingest RAW files from a source (e.g. an SD card) into the library.
//!
//! Modes: copy+add, move+add (verified before source deletion), reference (add-in-place).
//! Copy/move route into `‹library_root›/YYYY/YYYY-MM-DD/` by EXIF capture date, verify the
//! destination by content hash, handle filename collisions, and skip already-catalogued files.
//!
//! The DB mutex is held only for brief catalog writes — the slow per-file copy/hash/thumbnail work
//! runs UNLOCKED so concurrent IPC (library queries, etc.) stays responsive during a long import.

pub mod error;

pub use error::ImportError;

use chrono::DateTime;
use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    image_by_id, insert_image, now_epoch, process_file, relink_missing_image, ImageRow,
    ProcessedImage, ThumbCache, THUMB_SIZE,
};
use core_raw::{hash_file, read_metadata, source_from_path};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    /// Move-mode files that were catalogued but whose original could NOT be sent to Trash
    /// (the library copy is intact; the source was left in place). Distinct from `failed`.
    pub source_retained: usize,
}

/// A trash context that deletes silently and without involving Finder.
///
/// On macOS the `trash` crate's default `DeleteMethod::Finder` shells out to `osascript` →
/// `tell application "Finder" to delete {…}` **once per call** — which plays the Trash sound,
/// spawns a subprocess, and pulls Finder forward (a focus change that repaints the WKWebView
/// white). Across a Move import of N files that becomes N sounds + N flashes + N subprocesses.
/// `NsFileManager` uses `NSFileManager.trashItemAtURL` directly: silent, no subprocess, no focus
/// change, faster. Files still land in the Trash (recoverable by dragging out); they only lose the
/// one-click "Put Back" affordance.
fn make_trash_ctx() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
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

/// Content hashes of `present` rows, preloaded to skip already-catalogued files. Only `present`
/// rows pre-skip: a `missing` row (its original was deleted) must NOT short-circuit a re-import —
/// `relink_missing_image` relinks that row to the freshly-imported copy instead.
fn preload_present_hashes(conn: &Connection) -> Result<HashSet<[u8; 32]>, ImportError> {
    let mut seen: HashSet<[u8; 32]> = HashSet::new();
    let mut stmt = conn.prepare("SELECT content_hash FROM images WHERE status = 'present'")?;
    let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
    for h in rows.flatten() {
        if h.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&h);
            seen.insert(a);
        }
    }
    Ok(seen)
}

/// Outcome of the unlocked per-file processing phase, consumed by the (briefly-locked) catalog step.
enum Outcome {
    /// Content already `present` in the catalog (pre-copy hash match) — nothing was copied.
    Skip,
    /// A byte-identical file already sits at the destination — skip, but remember the hash so a
    /// later duplicate on the same card short-circuits before copying.
    SkipSeen([u8; 32]),
    /// Copied (or referenced) + processed; ready to catalog. `src_to_trash` is `Some` (Move mode,
    /// after a hash-verified copy) and is trashed by the caller *only after* the row is committed.
    /// `processed` is boxed — it dwarfs the other variants, so inline it would bloat every `Outcome`.
    Ready {
        processed: Box<ProcessedImage>,
        src_hash: [u8; 32],
        src_to_trash: Option<PathBuf>,
    },
}

/// Run an import. `progress(done, total, added)` fires per file; `added` is the freshly-inserted
/// row when that file was added to the catalog (`None` for skips/failures), letting callers stream
/// new images to the UI live. When `recursive` is false only the top-level of `source` is scanned;
/// when true the whole subtree is walked.
///
/// `db` is locked only briefly — once up front (folder row + present-hash snapshot + session), once
/// per catalogued file (relink/insert + session stamp + row read-back), and once at the end (session
/// finish). The copy/hash/thumbnail work between locks runs unlocked.
#[allow(clippy::too_many_arguments)]
pub fn import<F>(
    db: &Mutex<Db>,
    thumbs: &ThumbCache,
    source: &Path,
    mode: ImportMode,
    library_root: &Path,
    recursive: bool,
    progress: F,
) -> Result<ImportStats, ImportError>
where
    F: Fn(usize, usize, Option<&ImageRow>),
{
    let files = core_library::enumerate_raws(source, recursive);
    let total = files.len();

    // Brief lock: destination folder row (copy/move = library root; reference = the source),
    // present-hash snapshot, and the session row.
    let (folder_id, mut seen, session_id) = {
        let guard = db.lock().expect("import: db mutex poisoned");
        let conn = &guard.conn;
        let folder_id = match mode {
            ImportMode::Reference => core_library::add_root(conn, source)?,
            _ => core_library::add_root(conn, library_root)?,
        };
        let seen = preload_present_hashes(conn)?;
        let session_id = create_session(conn, &source.display().to_string(), mode)?;
        (folder_id, seen, session_id)
    };

    let trash_ctx = make_trash_ctx();
    let mut stats = ImportStats {
        session_id,
        total,
        ..Default::default()
    };
    let imported_at = now_epoch();

    for (i, src_path) in files.iter().enumerate() {
        // Unlocked: hash, dedup-check, copy, verify, thumbnail/metadata. A per-file error here is
        // recorded and the import continues (a single bad file must not abort the whole run).
        let outcome =
            match process_one_unlocked(thumbs, &trash_ctx, src_path, mode, library_root, &seen) {
                Ok(o) => o,
                Err(_) => {
                    stats.failed += 1;
                    progress(i + 1, total, None);
                    continue;
                }
            };

        let row = match outcome {
            Outcome::Skip => {
                stats.skipped += 1;
                None
            }
            Outcome::SkipSeen(h) => {
                seen.insert(h);
                stats.skipped += 1;
                None
            }
            Outcome::Ready {
                processed,
                src_hash,
                src_to_trash,
            } => {
                // Brief lock: recover a deleted-then-re-imported file by relinking its `missing`
                // row (keeps id + edits/keywords), else insert fresh; stamp the session; read the
                // row back for the live grid update.
                let inserted: Result<Option<(i64, Option<ImageRow>)>, ImportError> = (|| {
                    let guard = db.lock().expect("import: db mutex poisoned");
                    let conn = &guard.conn;
                    let id = match relink_missing_image(conn, folder_id, imported_at, &processed)? {
                        Some(id) => Some(id),
                        None => insert_image(conn, folder_id, imported_at, &processed)?,
                    };
                    match id {
                        Some(id) => {
                            conn.execute(
                                "UPDATE images SET import_session_id=?1 WHERE id=?2",
                                params![session_id, id],
                            )?;
                            // Read-back is best-effort: a failure only costs the live update.
                            Ok(Some((id, image_by_id(conn, id).ok().flatten())))
                        }
                        None => Ok(None),
                    }
                })(
                );

                match inserted {
                    Ok(Some((_id, row))) => {
                        stats.added += 1;
                        seen.insert(src_hash);
                        // Move: send the original to Trash ONLY now that its copy is durably
                        // catalogued. A trash failure leaves the source in place (counted, not lost).
                        if let Some(src) = src_to_trash {
                            if trash_ctx.delete(&src).is_err() {
                                stats.source_retained += 1;
                            }
                        }
                        row
                    }
                    Ok(None) => {
                        stats.skipped += 1;
                        None
                    }
                    Err(_) => {
                        stats.failed += 1;
                        None
                    }
                }
            }
        };
        progress(i + 1, total, row.as_ref());
    }

    {
        let guard = db.lock().expect("import: db mutex poisoned");
        finish_session(&guard.conn, &stats)?;
    }
    Ok(stats)
}

/// Unlocked per-file work: hash → dedup-check → (copy + hash-verify) → thumbnail/metadata. Touches
/// only the filesystem + CPU; never the DB. Returns what the caller should catalog (or skip).
fn process_one_unlocked(
    thumbs: &ThumbCache,
    trash_ctx: &trash::TrashContext,
    src_path: &Path,
    mode: ImportMode,
    library_root: &Path,
    seen: &HashSet<[u8; 32]>,
) -> Result<Outcome, ImportError> {
    let (src_hash, _size) = hash_file(src_path)?;
    if seen.contains(&src_hash) {
        return Ok(Outcome::Skip); // already in library (or imported this run)
    }

    let (dest_path, src_to_trash) = match mode {
        ImportMode::Reference => (src_path.to_path_buf(), None),
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
                    return Ok(Outcome::SkipSeen(src_hash));
                }
            }
            let dest = unique_dest(&dest_dir, filename);

            std::fs::copy(src_path, &dest)?;
            let (vh, _) = hash_file(&dest)?;
            if vh != src_hash {
                // Verification failed — remove the bad copy (silently), preserve the source, fail.
                let _ = trash_ctx.delete(&dest);
                return Err(ImportError::Io(std::io::Error::other(
                    "destination hash mismatch after copy",
                )));
            }
            let to_trash = if matches!(mode, ImportMode::Move) {
                Some(src_path.to_path_buf())
            } else {
                None
            };
            (dest, to_trash)
        }
    };

    let processed = process_file(&dest_path, thumbs, THUMB_SIZE)?;
    Ok(Outcome::Ready {
        processed: Box::new(processed),
        src_hash,
        src_to_trash,
    })
}
