//! Why a file is NOT in the catalog: the `decode_failure` table (schema 25) and its retry rule.
//!
//! Indexing used to record a decode failure as a number (`IndexStats.failed`) and nothing else. The
//! file was invisible in the UI, and every scan / watcher wake re-read and re-decoded it — a card
//! full of unknown-body RAWs cost the same work forever, with nothing to show for it.
//!
//! Every failed path now lands here with a typed [`core_raw::FailureKind`], rawler's own camera
//! identification, and the [`core_raw::DECODER_VERSION`] that produced the verdict. Two rules keep
//! the record honest instead of sticky:
//!
//! * **Retry after a decoder upgrade** — a row written by an older `decoder_version` is never
//!   skipped, so bumping rawler silently re-tries every file it previously refused.
//! * **Retry when the file changes** — the row stores `(file_size, mtime)`; a re-copied or repaired
//!   file no longer matches and is decoded again.
//!
//! Everything else (an unknown body, an unsupported compression) is skipped on subsequent scans:
//! that is the entire point of the table.

use crate::error::LibError;
use core_db::rusqlite::{named_params, Connection};
use core_raw::{FailureKind, RawError};
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

/// One recorded failure, as the Unsupported list renders it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodeFailureRow {
    /// Absolute path of the file that could not be catalogued (the row key — these files have no
    /// `images` row by definition).
    pub path: String,
    pub filename: String,
    /// `core_raw::FailureKind`, lowercase: `unsupported | corrupt | io | panic | other`.
    pub kind: String,
    /// Camera as rawler identified it; empty when the failure carried no identification.
    pub make: Option<String>,
    pub model: Option<String>,
    /// One sentence a user can act on (`RawError::user_detail`).
    pub detail: String,
    pub file_size: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub attempts: i64,
    /// Decoder build that produced this verdict; a newer build retries the file.
    pub decoder_version: String,
}

/// Failure tallies for the sidebar badge. `io` and `panic` fold into `other` — the user-facing split
/// that matters is "this build will never open it" vs "these bytes are damaged" vs "something else".
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecodeFailureCounts {
    pub unsupported: i64,
    pub corrupt: i64,
    pub other: i64,
}

impl DecodeFailureCounts {
    pub fn total(&self) -> i64 {
        self.unsupported + self.corrupt + self.other
    }
}

/// What the catalog stores about one failure, extracted from whatever error type the caller holds.
/// A separate struct because three different error types ([`LibError`], `ImportError`, a bare
/// [`RawError`]) all funnel into the same row.
#[derive(Debug, Clone)]
pub struct FailureFacts {
    pub kind: FailureKind,
    pub make: Option<String>,
    pub model: Option<String>,
    pub detail: String,
}

impl FailureFacts {
    /// Facts for a RAW-decode failure — the typed kind plus rawler's camera identification.
    pub fn from_raw(err: &RawError) -> Self {
        let (make, model) = match err.camera() {
            Some((make, model)) => (Some(make.to_string()), Some(model.to_string())),
            None => (None, None),
        };
        Self {
            kind: err.kind(),
            make,
            model,
            detail: err.user_detail(),
        }
    }

    /// Facts for an error that *may* wrap a [`RawError`]. `fallback_kind` classifies the rest (an
    /// I/O error is `Io`, anything else `Other`) and `detail` is that error's `Display` text.
    pub fn new(raw: Option<&RawError>, fallback_kind: FailureKind, detail: String) -> Self {
        match raw {
            Some(e) => Self::from_raw(e),
            None => Self {
                kind: fallback_kind,
                make: None,
                model: None,
                detail,
            },
        }
    }

    /// Facts for a library-side indexing failure.
    pub fn from_lib(err: &LibError) -> Self {
        let fallback = match err {
            LibError::Io(_) => FailureKind::Io,
            _ => FailureKind::Other,
        };
        Self::new(err.as_raw(), fallback, err.to_string())
    }
}

/// Lowercase discriminant matching the `kind` CHECK constraint (and `FailureKind`'s serde form).
fn kind_str(kind: FailureKind) -> &'static str {
    match kind {
        FailureKind::Unsupported => "unsupported",
        FailureKind::Corrupt => "corrupt",
        FailureKind::Io => "io",
        FailureKind::Panic => "panic",
        FailureKind::Other => "other",
    }
}

/// `(size, mtime)` of `path`, or `None` when it cannot be stat'd (deleted, unreadable).
fn file_stat(path: &Path) -> Option<(i64, Option<i64>)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    Some((md.len() as i64, mtime))
}

/// Record (or refresh) the failure for `path`. Returns `true` when this file is permanently
/// unsupported by this build — the caller's `unsupported` tally.
///
/// Upsert semantics: `attempts` increments and `last_seen` moves forward, `first_seen` is preserved
/// (so "known bad since" survives), and every other column is refreshed — a file whose verdict
/// changed (corrupt → unsupported after a repair, say) reads as its latest verdict, not its first.
pub fn record_decode_failure(
    conn: &Connection,
    folder_id: Option<i64>,
    path: &Path,
    err: &LibError,
    at: i64,
) -> Result<bool, LibError> {
    record_failure_facts(conn, folder_id, path, &FailureFacts::from_lib(err), at)
}

/// [`record_decode_failure`] for callers holding an error type this crate does not know
/// (`core-import`'s `ImportError`, or a bare [`RawError`] from a pre-copy metadata probe).
pub fn record_failure_facts(
    conn: &Connection,
    folder_id: Option<i64>,
    path: &Path,
    facts: &FailureFacts,
    at: i64,
) -> Result<bool, LibError> {
    let (file_size, mtime) = file_stat(path).unwrap_or((0, None));
    conn.execute(
        "INSERT INTO decode_failure(
             path, folder_id, filename, kind, make, model, detail,
             file_size, mtime, decoder_version, first_seen, last_seen, attempts
         ) VALUES (
             :path, :folder_id, :filename, :kind, :make, :model, :detail,
             :file_size, :mtime, :decoder_version, :at, :at, 1
         )
         ON CONFLICT(path) DO UPDATE SET
             folder_id       = excluded.folder_id,
             filename        = excluded.filename,
             kind            = excluded.kind,
             make            = excluded.make,
             model           = excluded.model,
             detail          = excluded.detail,
             file_size       = excluded.file_size,
             mtime           = excluded.mtime,
             decoder_version = excluded.decoder_version,
             last_seen       = excluded.last_seen,
             attempts        = decode_failure.attempts + 1",
        named_params! {
            ":path": path.display().to_string(),
            ":folder_id": folder_id,
            ":filename": path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            ":kind": kind_str(facts.kind),
            ":make": facts.make.as_deref().filter(|s| !s.is_empty()),
            ":model": facts.model.as_deref().filter(|s| !s.is_empty()),
            ":detail": facts.detail,
            ":file_size": file_size,
            ":mtime": mtime,
            ":decoder_version": core_raw::DECODER_VERSION,
            ":at": at,
        },
    )?;
    Ok(facts.kind == FailureKind::Unsupported)
}

/// Forget the failure recorded for `path` — called whenever the file *does* index, so a repaired or
/// re-copied file leaves no stale "unsupported" row behind.
pub fn clear_decode_failure(conn: &Connection, path: &str) -> Result<(), LibError> {
    conn.execute(
        "DELETE FROM decode_failure WHERE path = :path",
        named_params! { ":path": path },
    )?;
    Ok(())
}

/// Paths whose recorded failure still stands, and which a scan may therefore skip without opening
/// the file. A row is skippable only when **all** of:
///
/// * it was written by the current [`core_raw::DECODER_VERSION`] (a decoder upgrade retries
///   everything), and
/// * the file on disk still has the recorded `(file_size, mtime)` (a changed file is retried).
///
/// A path that cannot be stat'd (deleted, or on an unmounted card) is deliberately NOT skippable:
/// it will not be enumerated anyway, and if it comes back it deserves a fresh attempt.
pub fn skippable_failures(conn: &Connection) -> Result<HashSet<String>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT path, file_size, mtime FROM decode_failure WHERE decoder_version = :version",
    )?;
    let rows = stmt.query_map(
        named_params! { ":version": core_raw::DECODER_VERSION },
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?,
            ))
        },
    )?;
    let mut out = HashSet::new();
    for (path, size, mtime) in rows.flatten() {
        if file_stat(Path::new(&path)) == Some((size, mtime)) {
            out.insert(path);
        }
    }
    Ok(out)
}

/// Recorded failures, most recently seen first. `limit` caps the list the UI renders.
pub fn list_decode_failures(
    conn: &Connection,
    limit: i64,
) -> Result<Vec<DecodeFailureRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT path, filename, kind, make, model, detail, file_size,
                first_seen, last_seen, attempts, decoder_version
           FROM decode_failure
          ORDER BY last_seen DESC, path
          LIMIT :limit",
    )?;
    let rows = stmt.query_map(named_params! { ":limit": limit }, |r| {
        Ok(DecodeFailureRow {
            path: r.get(0)?,
            filename: r.get(1)?,
            kind: r.get(2)?,
            make: r.get(3)?,
            model: r.get(4)?,
            detail: r.get(5)?,
            file_size: r.get(6)?,
            first_seen: r.get(7)?,
            last_seen: r.get(8)?,
            attempts: r.get(9)?,
            decoder_version: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Failure tallies by class, for the sidebar badge.
pub fn decode_failure_counts(conn: &Connection) -> Result<DecodeFailureCounts, LibError> {
    let mut stmt = conn.prepare("SELECT kind, COUNT(*) FROM decode_failure GROUP BY kind")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut counts = DecodeFailureCounts::default();
    for (kind, n) in rows.flatten() {
        match kind.as_str() {
            "unsupported" => counts.unsupported += n,
            "corrupt" => counts.corrupt += n,
            _ => counts.other += n,
        }
    }
    Ok(counts)
}

/// Drop the recorded failures for `paths` so the next scan decodes them again — the "Try again"
/// action. Returns how many rows were actually removed.
pub fn forget_decode_failures(conn: &Connection, paths: &[String]) -> Result<usize, LibError> {
    let mut stmt = conn.prepare("DELETE FROM decode_failure WHERE path = :path")?;
    let mut removed = 0;
    for path in paths {
        removed += stmt.execute(named_params! { ":path": path })?;
    }
    Ok(removed)
}
