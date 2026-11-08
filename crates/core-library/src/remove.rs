//! "Remove from library": drop catalog rows only, never touch files.
//!
//! Sidecar `.json` files stay beside the originals, so a re-import restores edits and ratings.
//! Dependents (edits, keywords, collection links, outbox intents, pair links) go via
//! `ON DELETE CASCADE`. A RAW's camera companions go with it, as in [`crate::reject`], so no
//! paired JPEG/HEIF is left behind as a suddenly-visible orphan.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};

pub use core_model::catalog::RemoveResult;

/// Delete the rows of `ids` (unknown ids ignored) plus their companions, in one transaction.
pub fn remove_from_catalog(conn: &Connection, ids: &[i64]) -> Result<RemoveResult, LibError> {
    let tx = conn.unchecked_transaction()?;
    let mut unique: Vec<i64> = ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    let mut targets: Vec<i64> = Vec::new();
    for id in unique {
        if row_exists(&tx, id)? {
            targets.push(id);
        }
    }
    let primaries = targets.len();
    for i in 0..primaries {
        for sec in crate::pairs::secondaries_of(&tx, targets[i])? {
            if !targets.contains(&sec) {
                targets.push(sec);
            }
        }
    }
    let mut removed = 0usize;
    {
        let mut del = tx.prepare("DELETE FROM images WHERE id = ?1")?;
        for id in &targets {
            removed += del.execute(params![id])?;
        }
    }
    tx.commit()?;
    Ok(RemoveResult {
        removed,
        companions: targets.len() - primaries,
    })
}

fn row_exists(conn: &Connection, id: i64) -> Result<bool, LibError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM images WHERE id = ?1",
            params![id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
