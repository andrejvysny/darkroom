//! core-import — ingest RAW files from a source (e.g. an SD card) into the library.
//!
//! Modes: copy+add, move+add (verified before source deletion), reference (add-in-place).
//! Copy/move route into `‹library_root›/YYYY/YYYY-MM-DD/` by EXIF capture date, verify the
//! destination by content hash, handle filename collisions, and skip already-catalogued files.
//!
//! The DB mutex is held only for brief catalog writes — the slow per-file copy/hash/thumbnail work
//! runs UNLOCKED so concurrent IPC (library queries, etc.) stays responsive during a long import.

pub mod error;
pub mod pair;

pub use error::ImportError;
pub use pair::{detect_pairs, pair_roles, PairGroup, Pairing};

use chrono::DateTime;
use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    image_by_id, insert_image, now_epoch, process_bytes, relink_missing_image, FailureFacts,
    ImageRow, ProcessedImage, ThumbCache, THUMB_SIZE,
};
use core_raw::{content_hash, hash_file, read_metadata, source_from_bytes};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// Camera companions (JPEG/HEIF) linked to their RAW — only non-zero under [`Pairing::Pair`].
    pub paired: usize,
    /// Files this build can never decode (unknown body, unsupported compression). A strict subset
    /// of `failed`; each is recorded in `decode_failure` and, crucially, never copied into the
    /// library — an unreadable body leaves nothing behind.
    pub unsupported: usize,
}

/// Content-hash dedup classification of a source file. `Pending` is the listing default; the real
/// status is resolved by [`dedup_scan`] (BLAKE3 of the file vs the catalog + the rest of the batch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceStatus {
    /// Not yet hash-checked (just listed).
    Pending,
    /// Content hash absent from the catalog and unique in this batch.
    New,
    /// Content hash already `present` in the catalog (exact byte match).
    DuplicateLibrary,
    /// Identical content already appeared earlier in this same batch.
    DuplicateBatch,
}

/// One source file as listed by [`list_source`], from filesystem metadata ONLY (no file read, hash,
/// or decode — so listing a full card is instant). Status starts `Pending`; dedup runs in the
/// background. The thumbnail is loaded lazily, per file, on demand (`import_thumb`), never up front.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    /// Absolute source path (the commit selection key).
    pub path: String,
    pub filename: String,
    pub size_bytes: i64,
    /// File modification time (epoch seconds) — a fast stand-in for capture date in the list. The
    /// real EXIF capture date is read at commit time for on-disk date routing.
    pub mtime: i64,
    pub status: SourceStatus,
    /// Source format bucket ("raw" | "jpeg" | "png") — drives the Import dialog's by-type filter.
    pub kind: String,
    /// Identity of the RAW+JPEG/HEIF group this file belongs to (`None` when unpaired). Members of
    /// one group share the key, so the dialog can select/deselect a pair as a unit.
    pub pair_key: Option<String>,
    /// `"primary"` (the RAW) or `"secondary"` (its camera companion); `None` when unpaired.
    pub pair_role: Option<String>,
}

/// A resolved dedup verdict for one path (the output of [`dedup_scan`]).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupResult {
    pub path: String,
    pub status: SourceStatus,
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
/// Public: also used by the `hdr_merge` command to route a generated merged-HDR EXR.
pub fn date_subpath(epoch: i64) -> String {
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
/// Public: also used by the `hdr_merge` command for generated merged-HDR EXRs.
pub fn unique_dest(dir: &Path, filename: &str) -> PathBuf {
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
    /// Content already `present` in the catalog (pre-copy hash match) — nothing was copied. Carries
    /// the hash so pairing can still resolve this path to its existing catalog row.
    Skip([u8; 32]),
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
    /// This build can never decode the file — determined by the pre-copy metadata read, so NOTHING
    /// was copied. Recorded against the source path; the source is left exactly where it is (a Move
    /// import never trashes an original we could not read).
    Unsupported(core_raw::RawError),
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
    pairing: Pairing,
    progress: F,
) -> Result<ImportStats, ImportError>
where
    F: Fn(usize, usize, Option<&ImageRow>),
{
    let files = core_library::enumerate_raws(source, recursive);
    import_files(
        db,
        thumbs,
        source,
        &files,
        mode,
        library_root,
        pairing,
        progress,
    )
}

/// Number of source files handled by one parallel batch. Small enough that the catalog step (and
/// with it `seen`, the running duplicate set) advances often — a whole chunk is processed before any
/// of its hashes are known to the rest of the run — and large enough to keep the pool busy.
const CHUNK: usize = 16;

/// Worker count for the unlocked per-file phase. Reference mode is pure CPU (decode + thumbnail), so
/// it takes every core. Copy/Move additionally streams the file through the destination volume;
/// past ~4 concurrent writers a card reader or spinning disk slows down instead of speeding up, and
/// the extra threads only inflate peak memory (each holds a whole RAW in a buffer).
///
/// `DARKROOM_IMPORT_WORKERS`, when set to a positive integer, overrides the computed value outright
/// (any mode). Unset, non-numeric, or zero falls through to the default below. This is a pure
/// override — `bench_import` uses it to force a worker count per benchmark run; a future
/// user-facing setting is expected to reuse the same knob.
fn worker_count(mode: ImportMode) -> usize {
    if let Some(n) = std::env::var("DARKROOM_IMPORT_WORKERS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    match mode {
        ImportMode::Reference => cores,
        ImportMode::Copy | ImportMode::Move => cores.min(4),
    }
}

/// Run-constant inputs of the catalog step, so the per-file helpers stay short.
struct CommitCtx<'a> {
    db: &'a Mutex<Db>,
    mode: ImportMode,
    folder_id: i64,
    session_id: i64,
    imported_at: i64,
    pairing_on: bool,
    trash: &'a trash::TrashContext,
}

/// Mutable state threaded through the catalog step, in file order.
#[derive(Default)]
struct RunState {
    stats: ImportStats,
    /// Content hashes already accounted for. Grows here and ONLY here: the parallel phase reads a
    /// snapshot, so two identical files inside one chunk both get processed and the second is
    /// caught by the catalog step.
    seen: HashSet<[u8; 32]>,
    /// Source path → catalog row for rows added this run, for the pairing pass.
    new_ids: HashMap<PathBuf, i64>,
    /// Source path → content hash for files that were NOT added (already catalogued, or a duplicate
    /// of an earlier file in this run) — so a companion JPEG still pairs with a RAW that was
    /// skipped, or that arrived in an earlier import.
    src_hashes: HashMap<PathBuf, [u8; 32]>,
}

/// Import an explicit list of source files — the staged-preview commit path. Shares every per-file
/// catalog rule with [`import`] (which is just the "enumerate the whole source" wrapper). `source`
/// labels the import session and, in Reference mode, becomes the watched root.
///
/// Files are handled a [`CHUNK`] at a time: the unlocked read/copy/verify/decode/thumbnail work runs
/// in parallel across the chunk, then the catalog step replays the chunk's outcomes **in input
/// order** under brief locks — so row order, progress order, and every dedup rule are exactly what
/// the old file-at-a-time loop produced.
#[allow(clippy::too_many_arguments)]
pub fn import_files<F>(
    db: &Mutex<Db>,
    thumbs: &ThumbCache,
    source: &Path,
    files: &[PathBuf],
    mode: ImportMode,
    library_root: &Path,
    pairing: Pairing,
    progress: F,
) -> Result<ImportStats, ImportError>
where
    F: Fn(usize, usize, Option<&ImageRow>),
{
    let total = files.len();
    // Built before anything is written to the catalog: a pool that cannot be created must not leave
    // a half-open import session behind.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_count(mode))
        .build()
        .map_err(|e| ImportError::Io(std::io::Error::other(e.to_string())))?;

    // Brief lock: destination folder row (copy/move = library root; reference = the source),
    // present-hash snapshot, and the session row.
    let (folder_id, seen, session_id) = {
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
    let ctx = CommitCtx {
        db,
        mode,
        folder_id,
        session_id,
        imported_at: now_epoch(),
        pairing_on: pairing == Pairing::Pair,
        trash: &trash_ctx,
    };
    let mut st = RunState {
        stats: ImportStats {
            session_id,
            total,
            ..Default::default()
        },
        seen,
        ..Default::default()
    };
    // Destination names reserved by this run. Two source folders can hold the same filename, and in
    // parallel neither copy exists yet when the other picks its name — without a shared claim both
    // would land on the same path and one would overwrite the other.
    let claimed: Mutex<HashSet<PathBuf>> = Mutex::new(HashSet::new());

    let mut done = 0usize;
    for chunk in files.chunks(CHUNK) {
        // Unlocked + parallel: hash, dedup-check, copy, verify, thumbnail/metadata. A per-file error
        // is recorded below and the import continues (a single bad file must not abort the run).
        let outcomes: Vec<Result<Outcome, ImportError>> = pool.install(|| {
            chunk
                .par_iter()
                .map(|p| process_one_unlocked(thumbs, p, mode, library_root, &st.seen, &claimed))
                .collect()
        });
        for (src_path, outcome) in chunk.iter().zip(outcomes) {
            let row = commit_one(&ctx, &mut st, src_path, outcome);
            done += 1;
            progress(done, total, row.as_ref());
        }
    }

    if ctx.pairing_on {
        st.stats.paired = link_detected_pairs(db, files, &st.new_ids, &st.src_hashes);
    }

    {
        let guard = db.lock().expect("import: db mutex poisoned");
        finish_session(&guard.conn, &st.stats)?;
    }
    Ok(st.stats)
}

/// Catalog one file's outcome (brief locks only), advancing `st` in input order. Returns the freshly
/// inserted row for the live grid update, or `None` for a skip/failure.
fn commit_one(
    ctx: &CommitCtx<'_>,
    st: &mut RunState,
    src_path: &Path,
    outcome: Result<Outcome, ImportError>,
) -> Option<ImageRow> {
    let outcome = match outcome {
        Ok(o) => o,
        Err(e) => {
            st.stats.failed += 1;
            if record_source_failure(ctx.db, src_path, &failure_facts(&e), ctx.imported_at) {
                st.stats.unsupported += 1;
            }
            return None;
        }
    };

    match outcome {
        Outcome::Skip(h) => {
            if ctx.pairing_on {
                st.src_hashes.insert(src_path.to_path_buf(), h);
            }
            st.stats.skipped += 1;
            None
        }
        Outcome::SkipSeen(h) => {
            st.seen.insert(h);
            if ctx.pairing_on {
                st.src_hashes.insert(src_path.to_path_buf(), h);
            }
            st.stats.skipped += 1;
            None
        }
        Outcome::Unsupported(err) => {
            // No copy was made and the source is untouched — only the record is written.
            st.stats.failed += 1;
            st.stats.unsupported += 1;
            record_source_failure(
                ctx.db,
                src_path,
                &FailureFacts::from_raw(&err),
                ctx.imported_at,
            );
            None
        }
        Outcome::Ready {
            processed,
            src_hash,
            src_to_trash,
        } => commit_ready(ctx, st, src_path, *processed, src_hash, src_to_trash),
    }
}

/// Catalog a processed file: relink-or-insert, stamp the session, then (Move only) trash the source.
fn commit_ready(
    ctx: &CommitCtx<'_>,
    st: &mut RunState,
    src_path: &Path,
    processed: ProcessedImage,
    src_hash: [u8; 32],
    src_to_trash: Option<PathBuf>,
) -> Option<ImageRow> {
    // An earlier file of the SAME chunk carried identical bytes. The parallel phase saw `seen`
    // before that hash was recorded, so this file was copied+processed anyway; treat it exactly like
    // the pre-copy `Skip` it would have been, and delete the redundant copy. The source is never
    // trashed here — the file that "won" is a different original.
    if st.seen.contains(&src_hash) {
        if !matches!(ctx.mode, ImportMode::Reference) {
            remove_orphan_copy(Path::new(&processed.path));
        }
        if ctx.pairing_on {
            st.src_hashes.insert(src_path.to_path_buf(), src_hash);
        }
        st.stats.skipped += 1;
        return None;
    }

    match insert_catalog_row(ctx, src_path, &processed) {
        Ok(Some((id, row))) => {
            st.stats.added += 1;
            st.seen.insert(src_hash);
            if ctx.pairing_on {
                st.new_ids.insert(src_path.to_path_buf(), id);
            }
            // Move: send the original to Trash ONLY now that its copy is durably catalogued. A
            // trash failure leaves the source in place (counted, not lost).
            if let Some(src) = src_to_trash {
                if ctx.trash.delete(&src).is_err() {
                    st.stats.source_retained += 1;
                }
            }
            row
        }
        Ok(None) => {
            // The hash turned out to be in the catalog after all (it was absent from the `present`
            // snapshot this run started from). The copy is an orphan no rescan can adopt.
            if !matches!(ctx.mode, ImportMode::Reference) {
                remove_orphan_copy(Path::new(&processed.path));
            }
            st.stats.skipped += 1;
            None
        }
        Err(_) => {
            st.stats.failed += 1;
            None
        }
    }
}

/// Brief lock: recover a deleted-then-re-imported file by relinking its `missing` row (keeps id +
/// edits/keywords), else insert fresh; stamp the session; read the row back for the live grid
/// update. `Ok(None)` = a byte-identical row is already `present`.
fn insert_catalog_row(
    ctx: &CommitCtx<'_>,
    src_path: &Path,
    processed: &ProcessedImage,
) -> Result<Option<(i64, Option<ImageRow>)>, ImportError> {
    let guard = ctx.db.lock().expect("import: db mutex poisoned");
    let conn = &guard.conn;
    let id = match relink_missing_image(conn, ctx.folder_id, ctx.imported_at, processed)? {
        Some(id) => Some(id),
        None => insert_image(conn, ctx.folder_id, ctx.imported_at, processed)?,
    };
    let Some(id) = id else { return Ok(None) };
    conn.execute(
        "UPDATE images SET import_session_id=?1 WHERE id=?2",
        params![ctx.session_id, id],
    )?;
    // Restore edits/rating/keywords from a sidecar that travelled with the RAW (copied in
    // `process_one_unlocked`, or in place for reference mode).
    let _ = core_library::sidecar::hydrate_if_blank(conn, id, &processed.path);
    // The file indexed after all — drop any record of it failing, under both the source key (where
    // imports record) and the library copy's.
    let _ = core_library::clear_decode_failure(conn, &src_path.display().to_string());
    let _ = core_library::clear_decode_failure(conn, &processed.path);
    // Read-back is best-effort: a failure only costs the live update.
    Ok(Some((id, image_by_id(conn, id).ok().flatten())))
}

/// Delete a library copy that will never be catalogued, together with the sidecar copied beside it.
/// Leaving it behind puts a file in the library that no catalog row points at and that no rescan can
/// adopt. Reference mode owns nothing — callers must never invoke this on the user's own file.
fn remove_orphan_copy(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "import: failed to remove an orphan library copy"
        );
    }
    // The sidecar copied alongside it is just as orphaned.
    let _ = std::fs::remove_file(core_library::sidecar::sidecar_path(
        &path.display().to_string(),
    ));
}

/// Link each detected RAW+JPEG/HEIF group in `files` (one brief lock for the whole batch). Members
/// are resolved to catalog rows via this run's inserts first, then by content hash — so a companion
/// pairs with a RAW that was skipped as already-present, or imported earlier. A group whose primary
/// cannot be resolved (import failed, or the RAW was not selected) is silently left unpaired: a
/// failed link must never fail the import. Returns the number of links made.
fn link_detected_pairs(
    db: &Mutex<Db>,
    files: &[PathBuf],
    new_ids: &HashMap<PathBuf, i64>,
    src_hashes: &HashMap<PathBuf, [u8; 32]>,
) -> usize {
    let groups = pair::detect_pairs(files);
    if groups.is_empty() {
        return 0;
    }
    let guard = db.lock().expect("import: db mutex poisoned");
    let conn = &guard.conn;
    let resolve = |path: &PathBuf| -> Option<i64> {
        if let Some(id) = new_ids.get(path) {
            return Some(*id);
        }
        let hash = src_hashes.get(path)?;
        conn.query_row(
            "SELECT id FROM images WHERE content_hash = ?1 AND status = 'present'",
            params![&hash[..]],
            |r| r.get::<_, i64>(0),
        )
        .ok()
    };

    let mut linked = 0;
    for group in groups {
        let Some(primary_id) = resolve(&group.primary) else {
            continue;
        };
        for secondary in &group.secondaries {
            if let Some(secondary_id) = resolve(secondary) {
                if core_library::link_pair(conn, primary_id, secondary_id).unwrap_or(false) {
                    linked += 1;
                }
            }
        }
    }
    linked
}

/// List the importable RAW files under `source` from filesystem metadata ONLY — no file reads, no
/// hashing, no decode — so listing a whole card returns in milliseconds. Every file starts `Pending`;
/// [`dedup_scan`] resolves the real dedup status in the background. Thumbnails load lazily per file
/// via `import_thumb`.
pub fn list_source(source: &Path, recursive: bool) -> Vec<SourceFile> {
    let paths = core_library::enumerate_raws(source, recursive);
    // Path-only RAW+JPEG/HEIF grouping, so the dialog can offer the pairing choice up front.
    let roles = pair::pair_roles(&pair::detect_pairs(&paths));
    paths
        .into_iter()
        .map(|path| {
            let pair = roles.get(&path);
            SourceFile {
                filename: path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file.raw")
                    .to_string(),
                size_bytes: std::fs::metadata(&path)
                    .map(|m| m.len() as i64)
                    .unwrap_or(0),
                mtime: file_mtime_epoch(&path),
                status: SourceStatus::Pending,
                kind: core_library::image_kind(&path).to_string(),
                pair_key: pair.map(|(k, _)| k.clone()),
                pair_role: pair.map(|(_, r)| (*r).to_string()),
                path: path.display().to_string(),
            }
        })
        .collect()
}

/// Files hashed per [`dedup_scan`] batch — also the progress-callback granularity (unchanged).
const DEDUP_CHUNK: usize = 24;

/// Hashing workers for [`dedup_scan`]. Deliberately small: the scan runs in the background while the
/// user browses the staged card, and it is bound by reads from one (often slow) card reader.
const DEDUP_THREADS: usize = 4;

/// Hash-verify each path's dedup status against the catalog (`present_hashes`) and the rest of the
/// batch. **Size prefilter:** a file is only read+hashed when its size collides with a catalog file
/// or another batch file — a size unique everywhere can't be a byte-duplicate, so it's `New` with no
/// I/O. This keeps a full-card check to reading only the genuine candidates. `progress(done, total,
/// &newly_resolved)` fires periodically so the UI updates live.
///
/// Hashing is a pure read + BLAKE3 with no shared state, so a batch is hashed in parallel; the
/// verdicts are then assigned **sequentially in input order**, because "first occurrence wins /
/// later ones are `DuplicateBatch`" is order-dependent. Output order and verdicts are identical to
/// the fully sequential version.
pub fn dedup_scan<F>(
    paths: &[PathBuf],
    present_hashes: &HashSet<[u8; 32]>,
    present_sizes: &HashSet<i64>,
    progress: F,
) -> Vec<DedupResult>
where
    F: Fn(usize, usize, &[DedupResult]),
{
    let total = paths.len();

    // Size histogram of the batch (cheap fs metadata) — drives the "needs hashing?" prefilter.
    let mut size_of: Vec<i64> = Vec::with_capacity(total);
    let mut batch_size_count: std::collections::HashMap<i64, usize> =
        std::collections::HashMap::new();
    for p in paths {
        let sz = std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0);
        *batch_size_count.entry(sz).or_insert(0) += 1;
        size_of.push(sz);
    }
    let needs_hash = |i: usize| -> bool {
        let size = size_of[i];
        present_sizes.contains(&size) || batch_size_count.get(&size).copied().unwrap_or(0) > 1
    };

    // A private pool keeps a background scan off every core. If one cannot be built we simply hash
    // on the global pool — a thread-pool shortage must not fail a dedup preview.
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(DEDUP_THREADS)
        .build()
        .ok();

    let mut seen_batch: HashSet<[u8; 32]> = HashSet::new();
    let mut out: Vec<DedupResult> = Vec::with_capacity(total);
    let mut done = 0usize;

    for chunk in paths.chunks(DEDUP_CHUNK) {
        let base = done;
        // `None` = "not a hash candidate" (unique size) or "unreadable"; both classify as New.
        let hash_chunk = || -> Vec<Option<[u8; 32]>> {
            chunk
                .par_iter()
                .enumerate()
                .map(|(k, p)| {
                    if !needs_hash(base + k) {
                        return None;
                    }
                    // Unreadable here → New; the commit re-verifies and counts any real failure.
                    hash_file(p).ok().map(|(h, _)| h)
                })
                .collect()
        };
        let hashes = match &pool {
            Some(p) => p.install(hash_chunk),
            None => hash_chunk(),
        };

        let mut batch: Vec<DedupResult> = Vec::with_capacity(chunk.len());
        for (k, path) in chunk.iter().enumerate() {
            let status = match hashes[k] {
                None => SourceStatus::New,
                Some(h) => {
                    if present_hashes.contains(&h) {
                        SourceStatus::DuplicateLibrary
                    } else if !seen_batch.insert(h) {
                        SourceStatus::DuplicateBatch
                    } else {
                        SourceStatus::New
                    }
                }
            };
            batch.push(DedupResult {
                path: path.display().to_string(),
                status,
            });
        }
        done += chunk.len();
        progress(done, total, &batch);
        out.extend(batch);
    }
    out
}

/// Catalog facts for an import failure. `core-import`'s errors wrap a RAW decode failure one level
/// deeper than `core-library`'s, and an I/O failure is worth distinguishing from "something else".
fn failure_facts(err: &ImportError) -> FailureFacts {
    let fallback = match err {
        ImportError::Io(_) => core_raw::FailureKind::Io,
        _ => core_raw::FailureKind::Other,
    };
    FailureFacts::new(err.as_raw(), fallback, err.to_string())
}

/// Record a per-file import failure against the SOURCE path (`folder_id` is NULL — a card is not a
/// watched library folder). Returns `true` when the file is permanently unsupported. Best-effort:
/// neither the lock nor the write may fail an import that is otherwise fine.
fn record_source_failure(db: &Mutex<Db>, src_path: &Path, facts: &FailureFacts, at: i64) -> bool {
    let Ok(guard) = db.lock() else { return false };
    core_library::record_failure_facts(&guard.conn, None, src_path, facts, at).unwrap_or(false)
}

/// Reserve a destination path for `filename` inside `dir`, for this import run only.
///
/// Walks the same candidate sequence as [`unique_dest`] (`name`, `stem_1.ext`, `stem_2.ext`, …) but
/// additionally skips names another file of the same run already claimed. Files are copied in
/// parallel, so when two source folders hold the same filename NEITHER copy exists on disk yet when
/// the other picks its name — `!exists()` alone would hand both threads the same destination and one
/// copy would silently overwrite the other. The whole search runs under one lock so a name cannot be
/// claimed between the existence check and the insert.
fn claim_unique_dest(dir: &Path, filename: &str, claimed: &Mutex<HashSet<PathBuf>>) -> PathBuf {
    let mut taken = claimed.lock().expect("import: claimed-dest mutex poisoned");
    let path = Path::new(filename);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    for n in 0.. {
        let cand = match n {
            0 => dir.join(filename),
            _ if ext.is_empty() => dir.join(format!("{stem}_{n}")),
            _ => dir.join(format!("{stem}_{n}.{ext}")),
        };
        if !cand.exists() && taken.insert(cand.clone()) {
            return cand;
        }
    }
    unreachable!()
}

/// Unlocked per-file work: read → hash → dedup-check → (write + hash-verify) → thumbnail/metadata.
/// Touches only the filesystem + CPU; never the DB. Runs in parallel across a chunk, so `seen` is a
/// read-only snapshot (duplicates *within* a chunk are resolved by the catalog step) and every
/// destination name is reserved through `claimed`. Returns what the caller should catalog (or skip).
///
/// The source is read exactly ONCE and the buffer then serves the hash, the "can we open this?"
/// metadata probe, the bytes written to the destination, and the final metadata/thumbnail pass. The
/// only other full read is `hash_file` over the freshly written temp file — the on-disk verification
/// that the copy is byte-identical, which by definition cannot be done from memory.
fn process_one_unlocked(
    thumbs: &ThumbCache,
    src_path: &Path,
    mode: ImportMode,
    library_root: &Path,
    seen: &HashSet<[u8; 32]>,
    claimed: &Mutex<HashSet<PathBuf>>,
) -> Result<Outcome, ImportError> {
    let bytes = Arc::new(std::fs::read(src_path)?);
    let src_hash = content_hash(&bytes);
    if seen.contains(&src_hash) {
        return Ok(Outcome::Skip(src_hash)); // already in library (or imported this run)
    }

    let (dest_path, src_to_trash) = match mode {
        ImportMode::Reference => (src_path.to_path_buf(), None),
        ImportMode::Copy | ImportMode::Move => {
            // Resolve date folder, from the bytes we already hold.
            let src = source_from_bytes(bytes.clone(), src_path);
            // The metadata read doubles as the "can this build open the file at all?" probe. An
            // unknown body bails out HERE, before the copy — otherwise every unsupported RAW left an
            // orphan in `library/YYYY/YYYY-MM-DD/` that no catalog row ever pointed at. Any other
            // metadata failure keeps the old behaviour (fall back to mtime for date routing).
            let capture = match read_metadata(&src) {
                Ok(m) => m.capture_date.unwrap_or_else(|| file_mtime_epoch(src_path)),
                Err(e) if e.kind() == core_raw::FailureKind::Unsupported => {
                    return Ok(Outcome::Unsupported(e))
                }
                Err(_) => file_mtime_epoch(src_path),
            };
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
            let dest = claim_unique_dest(&dest_dir, filename, claimed);

            // Write to a temp sibling, hash-verify, then ATOMIC rename into place. A crash mid-copy
            // leaves an inert `*.part` file (not a supported RAW ext, so never enumerated/catalogued)
            // rather than a truncated file sitting at the real destination name. The temp name is
            // unique within the run because `dest` is.
            let tmp = {
                let mut t = dest.clone().into_os_string();
                t.push(".part");
                PathBuf::from(t)
            };
            std::fs::write(&tmp, bytes.as_slice())?;
            let (vh, _) = hash_file(&tmp)?;
            if vh != src_hash {
                // Verification failed — remove the bad temp copy, preserve the source, fail.
                let _ = std::fs::remove_file(&tmp);
                return Err(ImportError::Io(std::io::Error::other(
                    "destination hash mismatch after copy",
                )));
            }
            std::fs::rename(&tmp, &dest)?;
            // Bring along the source's sidecar (edit intent), if any, so the copy keeps its edits.
            let src_sidecar = core_library::sidecar::sidecar_path(&src_path.display().to_string());
            if src_sidecar.exists() {
                let dest_sidecar = core_library::sidecar::sidecar_path(&dest.display().to_string());
                let _ = std::fs::copy(&src_sidecar, &dest_sidecar);
            }
            let to_trash = if matches!(mode, ImportMode::Move) {
                Some(src_path.to_path_buf())
            } else {
                None
            };
            (dest, to_trash)
        }
    };

    // The destination is byte-identical to the source (verified above), so the buffer we already
    // hold IS the destination's content — no second read, and `src_hash` stays its content hash.
    let processed = match process_bytes(&dest_path, bytes, src_hash, thumbs, THUMB_SIZE) {
        Ok(p) => p,
        Err(e) => {
            // The copy exists but will never be catalogued. Reference mode owns nothing: never
            // touch the user's own file.
            if !matches!(mode, ImportMode::Reference) {
                remove_orphan_copy(&dest_path);
            }
            return Err(e.into());
        }
    };
    Ok(Outcome::Ready {
        processed: Box::new(processed),
        src_hash,
        src_to_trash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A body this build cannot decode must be recognised BEFORE the copy — otherwise every
    /// unsupported RAW leaves an orphan in `library/YYYY/YYYY-MM-DD/` that no catalog row points at
    /// and no rescan can adopt.
    ///
    /// Junk bytes under a RAW extension are enough: rawler reports "No decoder found", which
    /// `core-raw` classifies as `FailureKind::Unsupported` (verified, not assumed) — the same class
    /// a real unknown body produces.
    #[test]
    fn an_undecodable_source_is_refused_before_anything_is_copied() {
        let src_dir = tempfile::tempdir().unwrap();
        let library = tempfile::tempdir().unwrap();
        let thumbdir = tempfile::tempdir().unwrap();
        let thumbs = ThumbCache::new(thumbdir.path()).unwrap();

        let src = src_dir.path().join("DSC_0001.NEF");
        std::fs::write(&src, vec![0x37u8; 4096]).unwrap();

        let outcome = process_one_unlocked(
            &thumbs,
            &src,
            ImportMode::Copy,
            library.path(),
            &HashSet::new(),
            &Mutex::new(HashSet::new()),
        )
        .expect("an undecodable file is an outcome, not an error");

        match outcome {
            Outcome::Unsupported(e) => {
                assert_eq!(e.kind(), core_raw::FailureKind::Unsupported);
            }
            _ => panic!("expected Outcome::Unsupported"),
        }
        assert!(src.exists(), "the source must be left exactly where it was");
        assert_eq!(
            std::fs::read_dir(library.path()).unwrap().count(),
            0,
            "nothing may be written into the library for a file we cannot open"
        );
    }
}
