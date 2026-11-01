//! Sending catalogued files to the OS Trash and dropping their rows.
//!
//! The canonical trash primitive for the catalog: never a hard delete, and a row is removed only
//! once its file is verifiably gone, so the catalog can never point at a still-present file it
//! believes it deleted. (`core-dedup` keeps its own copy of this for the duplicate-resolve path; it
//! does not depend on this crate.)

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

/// A trash context that deletes silently and without involving Finder. On macOS the `trash` crate's
/// default `DeleteMethod::Finder` shells out to `osascript`/Finder per call — playing the Trash
/// sound, spawning a subprocess and pulling Finder forward (a white WKWebView repaint). Deleting N
/// files would otherwise fire that N times. `NsFileManager` trashes silently and directly; files
/// remain recoverable from the Trash (sans one-click "Put Back").
pub fn make_trash_ctx() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

/// Outcome of a trash pass. `hashes` are hex content-hashes of the removed images so the caller can
/// GC orphaned thumbnails (re-checking presence first — another row may share the hash).
#[derive(Debug, Default, Clone)]
pub struct TrashOutcome {
    /// Files trashed *and* rows removed.
    pub trashed: usize,
    /// Files that could not be trashed (permissions, locked volume); their rows were left intact.
    pub failed: usize,
    /// Trashed file whose row was repointed or replaced before finalize; the row was left untouched.
    pub changed: usize,
    pub hashes: Vec<String>,
}

/// One file the catalog intends to trash, as it was when planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashTarget {
    pub id: i64,
    pub path: String,
    pub hash_hex: String,
}

/// Brief lock: snapshot (id, path, content hash) for each id; ids without a row are skipped.
pub fn plan_trash(conn: &Connection, ids: &[i64]) -> Result<Vec<TrashTarget>, LibError> {
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        let row: Option<(String, Vec<u8>)> = conn
            .query_row(
                "SELECT path, content_hash FROM images WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((path, hash)) = row {
            out.push(TrashTarget {
                id,
                path,
                hash_hex: hex_lower(&hash),
            });
        }
    }
    Ok(out)
}

/// No catalog access. Trash each target's file (and its `.json` sidecar, best-effort) with
/// `remove`; returns the targets whose file is now gone (already-missing counts as gone) and the
/// number that failed.
pub fn trash_files(
    targets: &[TrashTarget],
    remove: &dyn Fn(&Path) -> bool,
) -> (Vec<TrashTarget>, usize) {
    let mut gone = Vec::new();
    let mut failed = 0usize;
    for t in targets {
        let p = Path::new(&t.path);
        if p.exists() && !remove(p) {
            failed += 1;
            continue; // leave the row intact — the file is still there
        }
        // Best-effort: the sidecar carries only derived state, so a failure here is not fatal.
        let sidecar = crate::sidecar::sidecar_path(&t.path);
        if sidecar.exists() {
            remove(&sidecar);
        }
        gone.push(t.clone());
    }
    (gone, failed)
}

/// Brief lock: in one transaction delete each gone target's row ONLY if the row still has the same
/// path and content hash as planned (a row repointed/replaced meanwhile is kept and counted in
/// [`TrashOutcome::changed`]). Returns trashed (rows removed), failed (passed through), hashes of
/// removed rows.
pub fn finalize_trash(
    conn: &Connection,
    gone: &[TrashTarget],
    failed: usize,
) -> Result<TrashOutcome, LibError> {
    let mut out = TrashOutcome {
        failed,
        ..TrashOutcome::default()
    };
    let tx = conn.unchecked_transaction()?;
    {
        let mut del = tx.prepare(
            "DELETE FROM images WHERE id = ?1 AND path = ?2 AND lower(hex(content_hash)) = ?3",
        )?;
        let mut still = tx.prepare("SELECT 1 FROM images WHERE id = ?1")?;
        for t in gone {
            if del.execute(params![t.id, t.path, t.hash_hex])? > 0 {
                out.trashed += 1;
                out.hashes.push(t.hash_hex.clone());
                continue;
            }
            // Zero rows deleted: either the row vanished on its own (not our business) or its
            // path/hash moved under us — only the latter is a repoint we deliberately preserved.
            let kept: Option<i64> = still.query_row(params![t.id], |r| r.get(0)).optional()?;
            if kept.is_some() {
                out.changed += 1;
            }
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Send each image's file (and its `.json` sidecar) to the Trash, then drop the rows of those whose
/// file is gone, in one transaction. Rows already absent are ignored. Dependent rows (edits,
/// keywords, faces, pair links, …) fall away via `ON DELETE CASCADE`.
///
/// A file that fails to trash keeps its catalog row and is counted in `failed`.
pub fn trash_images(conn: &Connection, ids: &[i64]) -> Result<TrashOutcome, LibError> {
    let ctx = make_trash_ctx();
    trash_images_with(conn, ids, &move |p| ctx.delete(p).is_ok())
}

/// `trash_images` with an injectable remover — `remove` returns `true` when the file is gone.
/// Exists so tests can exercise the real file/row path without touching the user's Trash.
pub fn trash_images_with(
    conn: &Connection,
    ids: &[i64],
    remove: &dyn Fn(&Path) -> bool,
) -> Result<TrashOutcome, LibError> {
    let targets = plan_trash(conn, ids)?;
    let (gone, failed) = trash_files(&targets, remove);
    finalize_trash(conn, &gone, failed)
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
