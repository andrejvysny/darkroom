//! Bulk deletion of rejected photos ("empty the rejects").
//!
//! Safety model — the caller never supplies image ids. It supplies the *filter* the grid is showing;
//! this module re-derives the target set from the catalog with the `reject` flag forced on
//! (`query::rejected_ids`), so nothing outside that set can be deleted. The only rows added on top
//! are camera companions of a rejected RAW, which are counted separately and reported to the UI —
//! without them, deleting the RAW would leave its paired JPEG/HEIF behind as a suddenly-visible
//! orphan (the `image_pairs` link cascades away with the primary).
//!
//! Files go to the OS Trash, never a hard delete (see [`crate::trash`]).

use crate::error::LibError;
use crate::query::{rejected_ids, QueryParams};
use core_db::rusqlite::Connection;
use serde::Serialize;

/// What "delete all rejected" would touch, for the confirmation dialog.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectSummary {
    /// Images the user flagged `reject`.
    pub images: i64,
    /// Camera companions (paired JPEG/HEIF) that ride along with a rejected RAW.
    pub companions: i64,
    /// Total bytes on disk across both.
    pub bytes: i64,
}

/// Result of a delete pass.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectDeleteResult {
    /// Files trashed and rows removed (images + companions).
    pub trashed: usize,
    /// Files that could not be trashed; their rows were kept.
    pub failed: usize,
    /// Of `trashed`, how many were camera companions.
    pub companions: usize,
    /// Trashed file whose row was repointed or replaced before finalize; the row was left untouched.
    pub changed: usize,
    /// Hex content-hashes of the removed images (for thumbnail GC).
    pub hashes: Vec<String>,
}

/// Planned "delete rejected" run: targets plus which ids are companions (for the count).
pub struct RejectPlan {
    pub targets: Vec<crate::trash::TrashTarget>,
    pub companions: std::collections::HashSet<i64>,
}

/// Brief lock: derive the delete set for `p` and snapshot its files, so the file work can run with
/// the catalog lock released.
pub fn plan_delete_rejected(conn: &Connection, p: &QueryParams) -> Result<RejectPlan, LibError> {
    let (rejected, companions) = target_ids(conn, p)?;
    let all: Vec<i64> = rejected.iter().chain(companions.iter()).copied().collect();
    Ok(RejectPlan {
        targets: crate::trash::plan_trash(conn, &all)?,
        companions: companions.into_iter().collect(),
    })
}

/// Brief lock: remove the rows of `gone` targets whose file work succeeded, then report. Companion
/// count covers only rows actually removed.
pub fn finalize_delete_rejected(
    conn: &Connection,
    plan: &RejectPlan,
    gone: &[crate::trash::TrashTarget],
    failed: usize,
) -> Result<RejectDeleteResult, LibError> {
    let out = crate::trash::finalize_trash(conn, gone, failed)?;
    // Rows that survived (failed to trash, or repointed under us) are still present; count
    // companions among those actually gone.
    let companions_gone = plan
        .companions
        .iter()
        .filter(|&&id| !row_exists(conn, id).unwrap_or(true))
        .count();

    Ok(RejectDeleteResult {
        trashed: out.trashed,
        failed: out.failed,
        companions: companions_gone,
        changed: out.changed,
        hashes: out.hashes,
    })
}

/// The full delete set for `p`: rejected images plus the companions of any rejected primary.
/// Returns `(rejected, companions)` — disjoint, both sorted, companions excludes anything already
/// rejected in its own right.
fn target_ids(conn: &Connection, p: &QueryParams) -> Result<(Vec<i64>, Vec<i64>), LibError> {
    let rejected = rejected_ids(conn, p)?;
    if rejected.is_empty() {
        return Ok((rejected, Vec::new()));
    }
    let mut companions: Vec<i64> = Vec::new();
    for &id in &rejected {
        for sec in crate::pairs::secondaries_of(conn, id)? {
            if !rejected.contains(&sec) {
                companions.push(sec);
            }
        }
    }
    companions.sort_unstable();
    companions.dedup();
    Ok((rejected, companions))
}

/// Count + on-disk size of what [`delete_rejected`] would remove for the same `p`. Read-only.
pub fn summarize_rejected(conn: &Connection, p: &QueryParams) -> Result<RejectSummary, LibError> {
    let (rejected, companions) = target_ids(conn, p)?;
    let bytes = total_bytes(conn, &rejected)? + total_bytes(conn, &companions)?;
    Ok(RejectSummary {
        images: rejected.len() as i64,
        companions: companions.len() as i64,
        bytes,
    })
}

/// Trash every rejected image matching `p` (plus their camera companions) and drop their rows.
///
/// Not reversible from inside the app — files are recoverable from the OS Trash only.
pub fn delete_rejected(conn: &Connection, p: &QueryParams) -> Result<RejectDeleteResult, LibError> {
    let ctx = crate::trash::make_trash_ctx();
    delete_rejected_with(conn, p, &move |path| ctx.delete(path).is_ok())
}

/// `delete_rejected` with an injectable remover — `remove` returns `true` when the file is gone.
/// Exists so tests can exercise the full path without filling the developer's Trash.
pub fn delete_rejected_with(
    conn: &Connection,
    p: &QueryParams,
    remove: &dyn Fn(&std::path::Path) -> bool,
) -> Result<RejectDeleteResult, LibError> {
    let plan = plan_delete_rejected(conn, p)?;
    let (gone, failed) = crate::trash::trash_files(&plan.targets, remove);
    finalize_delete_rejected(conn, &plan, &gone, failed)
}

fn row_exists(conn: &Connection, id: i64) -> Result<bool, LibError> {
    use core_db::rusqlite::OptionalExtension;
    Ok(conn
        .query_row("SELECT 1 FROM images WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

fn total_bytes(conn: &Connection, ids: &[i64]) -> Result<i64, LibError> {
    let mut total = 0i64;
    let mut stmt = conn.prepare("SELECT file_size FROM images WHERE id = ?1")?;
    for &id in ids {
        use core_db::rusqlite::OptionalExtension;
        if let Some(n) = stmt.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
            total = total.saturating_add(n);
        }
    }
    Ok(total)
}
