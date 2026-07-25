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
    pub hashes: Vec<String>,
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
    let mut out = TrashOutcome::default();
    let mut to_delete: Vec<i64> = Vec::with_capacity(ids.len());

    for &id in ids {
        let row: Option<(String, Vec<u8>)> = conn
            .query_row(
                "SELECT path, content_hash FROM images WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((path, hash)) = row else { continue };

        let p = Path::new(&path);
        if p.exists() && !remove(p) {
            out.failed += 1;
            continue; // leave the row intact — the file is still there
        }
        // Best-effort: the sidecar carries only derived state, so a failure here is not fatal.
        let sidecar = crate::sidecar::sidecar_path(&path);
        if sidecar.exists() {
            remove(&sidecar);
        }
        to_delete.push(id);
        out.hashes.push(hex_lower(&hash));
    }

    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare("DELETE FROM images WHERE id = ?1")?;
        for id in &to_delete {
            stmt.execute(params![id])?;
        }
    }
    tx.commit()?;

    out.trashed = to_delete.len();
    Ok(out)
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
