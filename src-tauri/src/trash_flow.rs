//! Snapshot → unlocked file IO → fenced publish for the catalog-mutating trash commands
//! (`cull_delete_rejected`, `dedup_resolve`, `dedup_resolve_bulk`).
//!
//! The old shape took `publications.invalidate_all_guard()` **and** the catalog mutex for the whole
//! run, so every thumbnail/sidecar/analysis publication and every other reader stalled for the
//! duration of N Trash moves. Each command now runs three phases:
//!
//! 1. **Plan** (brief lock): snapshot the targets and retire their publication identities, so any
//!    in-flight thumbnail/sidecar/analysis work derived from them fails its fence from here on.
//! 2. **Work** (no locks): the actual file IO, with the catalog and the gate both released.
//! 3. **Publish** (brief lock): re-acquire a commit guard fenced on the plan's epoch, finalize the
//!    rows, and update **only the affected sources**: ids whose row was removed stay `retire`d,
//!    and ids whose row was kept (trash failed, repointed, keeper lost) are re-registered with
//!    their current identity via `replace_source`, so they are publishable again. The catalog
//!    **epoch** is left untouched — it changes only on a true reset (`database_reset` via
//!    `invalidate_all_guard`). A global `invalidate_all` here would look like a catalog reset to
//!    every `commit_guard(epoch)` fence and abort concurrent imports/indexes as stale after an
//!    ordinary cull or dedup delete.
//!
//! If the catalog was reset between plan and publish (Stale), the new catalog is left untouched:
//! finalized row deletes would target rows we never read. Files already in the Trash are
//! recoverable, so nothing is lost — the command reports the file counts and moves on.
//!
//! Lock order everywhere: publication gate before the catalog (see `PublicationGate`).

use crate::state::AppState;
use core_db::rusqlite::Connection;
use core_db::Db;
use core_model::identity::{CatalogEpoch, ImageId};
use core_runtime::publication::{PublicationGate, SourceKey};
use std::path::Path;
use std::sync::Mutex;

mod dedup;
pub(crate) use dedup::{run_resolve, run_resolve_bulk};

#[cfg(test)]
mod tests;

/// The catalog generation vanished between planning and (re)acquiring the commit guard.
const CHANGED: &str = "catalog changed; try again";

/// Outcome of a publish attempt.
pub(crate) enum Publish<T> {
    /// Fenced commit succeeded; payload plus the orphan-thumbnail hashes to delete after the locks
    /// are released.
    Published(T, Vec<String>),
    /// The catalog was reset while the file IO ran. The command must not touch the new catalog.
    Stale,
}

/// Content hashes from `hashes` that no present row references any more — safe to drop from the
/// thumbnail cache. Run under the catalog lock; the caller deletes the files after releasing it.
///
/// A re-import of the same bytes between this query and the unlocked removal at worst costs one
/// cache regeneration: thumbnails are a content-addressed cache, never source data.
pub(crate) fn orphan_thumb_hashes(conn: &Connection, hashes: &[String]) -> Vec<String> {
    use core_db::rusqlite::OptionalExtension;
    hashes
        .iter()
        .filter(|h| {
            conn.query_row(
                "SELECT 1 FROM images WHERE lower(hex(content_hash)) = ?1 AND status='present' LIMIT 1",
                [&**h],
                |_| Ok(()),
            )
            .optional()
            // A query error keeps the thumbnail — conservative, the cap evictor still bounds it.
            .map(|found| found.is_none())
            .unwrap_or(false)
        })
        .cloned()
        .collect()
}

/// Unlocked orphan-thumbnail removal (call after the catalog lock and gate are dropped).
pub(crate) fn remove_orphan_thumbs(thumbs: &core_library::ThumbCache, hashes: &[String]) {
    for h in hashes {
        let _ = thumbs.remove_hash(h);
    }
}

/// Re-sync the gate for every id the plan retired, under the publish guard + db lock, after
/// finalize: an id whose row was **removed** stays `retire`d (its entry is cleared so a stale
/// publisher can never capture it again); an id whose row was **kept** (trash failed, repointed,
/// keeper lost) is re-registered with its current path + hex content hash via `replace_source`,
/// so it is publishable again — the plan retired it, and nothing else would bring it back.
///
/// Reading the row's *current* values matters: a kept row may have been re-pointed meanwhile, and
/// publishing under the stale planned identity would describe a file that row no longer names.
fn refresh_planned_sources(
    gate: &mut core_runtime::publication::SourceMutationGuard<'_>,
    conn: &Connection,
    ids: impl IntoIterator<Item = i64>,
) -> Result<(), String> {
    use core_db::rusqlite::{params, OptionalExtension};
    for id in ids {
        let row: Option<(String, String)> = conn
            .query_row(
                "SELECT path, lower(hex(content_hash)) FROM images WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match row {
            Some((path, hash)) => gate.replace_source(ImageId(id), SourceKey { hash, path }),
            None => gate.retire(ImageId(id)),
        }
    }
    Ok(())
}

// ---------- cull_delete_rejected ----------

/// A planned "delete all rejected" run: the epoch it was derived on plus the file snapshot.
pub(crate) struct RejectRun {
    pub epoch: CatalogEpoch,
    pub plan: core_library::RejectPlan,
}

/// Plan phase under a brief lock: derive the delete set from the filter (never from caller ids)
/// and retire every target's publication identity.
pub(crate) fn plan_reject(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    params: &core_library::QueryParams,
) -> Result<RejectRun, String> {
    let epoch = publications.epoch();
    let mut gate = publications
        .commit_guard(epoch)
        .map_err(|_| CHANGED.to_string())?;
    let plan = {
        let db = db.lock().map_err(|e| e.to_string())?;
        core_library::plan_delete_rejected(&db.conn, params).map_err(|e| e.to_string())?
    };
    for t in &plan.targets {
        gate.retire(ImageId(t.id));
    }
    drop(gate);
    Ok(RejectRun { epoch, plan })
}

/// Work phase, no locks held: trash each planned file (+ sidecar) with `remove`; returns the
/// targets whose file is verifiably gone and the failure count.
pub(crate) fn trash_reject(
    plan: &core_library::RejectPlan,
    remove: &dyn Fn(&Path) -> bool,
) -> (Vec<core_library::trash::TrashTarget>, usize) {
    core_library::trash::trash_files(&plan.targets, remove)
}

/// Publish phase: finalize the gone rows fenced on the plan's epoch, re-sync only the planned
/// sources (removed ids retired, kept ids re-registered), compute orphan thumbnail hashes and
/// append the behavioural events — all under one lock, then release both locks before returning.
pub(crate) fn publish_reject(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    run: &RejectRun,
    gone: &[core_library::trash::TrashTarget],
    failed: usize,
    log: &dyn Fn(&Connection, &core_library::RejectDeleteResult),
) -> Result<Publish<core_library::RejectDeleteResult>, String> {
    let mut gate = match publications.commit_guard(run.epoch) {
        Ok(gate) => gate,
        Err(core_runtime::publication::Stale) => return Ok(Publish::Stale),
    };
    let db = db.lock().map_err(|e| e.to_string())?;
    let res = core_library::finalize_delete_rejected(&db.conn, &run.plan, gone, failed)
        .map_err(|e| e.to_string())?;
    // Per-source, not `invalidate_all`: the catalog was mutated, not replaced — a global epoch
    // bump would abort concurrent imports/indexes as stale (see module docs).
    refresh_planned_sources(&mut gate, &db.conn, run.plan.targets.iter().map(|t| t.id))?;
    let orphans = orphan_thumb_hashes(&db.conn, &res.hashes);
    log(&db.conn, &res);
    drop(db);
    drop(gate);
    Ok(Publish::Published(res, orphans))
}

/// Behavioural log for a rejected-delete pass, appended under the publish lock.
fn log_reject_delete(st: &AppState, conn: &Connection, res: &core_library::RejectDeleteResult) {
    let _ = core_library::append_event(
        conn,
        &crate::events::stamp(
            st,
            core_library::Event {
                event_type: "culling.delete_rejected".into(),
                flag: Some("reject".into()),
                touch_count: Some(res.trashed as i64),
                context: Some(
                    serde_json::json!({
                        "companions": res.companions,
                        "failed": res.failed,
                    })
                    .to_string(),
                ),
                ..Default::default()
            },
        ),
    );
}

/// Full `cull_delete_rejected` run: plan → unlock → trash → fenced publish. The frontend supplies
/// only the filter; the target set is re-derived from the catalog.
///
/// Serialized against flag mutations by the job queue, not a lock: `cull_set_flag{,_many}`,
/// `image_pair_unlink` and this command all run as `JobClass::UserCommand` jobs with
/// `QueuePolicy::Ordered` under the same owner, and the worker's candidate filter
/// (`crates/core-runtime/src/jobs/worker.rs`) never starts a second Running Ordered UserCommand
/// job for that owner. That is what keeps a photo from being un-rejected (or un-paired) between
/// this run's plan and finalize — the epoch fence catches resets, not culling edits.
pub(crate) fn run_delete_rejected(
    st: &AppState,
    params: &core_library::QueryParams,
) -> Result<core_library::RejectDeleteResult, String> {
    let run = plan_reject(&st.publications, &st.db, params)?;
    // Documented precondition of `clear_source_caches` (state.rs): the publication gate must be
    // held so reused row ids cannot retain another source's caches.
    st.publications.with_mutation(|| st.clear_source_caches());
    let ctx = core_library::trash::make_trash_ctx();
    let (gone, failed) = trash_reject(&run.plan, &|p| ctx.delete(p).is_ok());
    let published = publish_reject(
        &st.publications,
        &st.db,
        &run,
        &gone,
        failed,
        &|conn, res| log_reject_delete(st, conn, res),
    )?;
    match published {
        Publish::Stale => {
            // Files are in the Trash (recoverable); the reset already dropped their rows.
            tracing::warn!(
                files_trashed = gone.len(),
                failed,
                "catalog was reset while rejected files were being trashed; \
                 the new catalog is left untouched"
            );
            Ok(core_library::RejectDeleteResult {
                trashed: 0,
                failed,
                ..Default::default()
            })
        }
        Publish::Published(res, orphans) => {
            st.publications.with_mutation(|| st.clear_source_caches());
            remove_orphan_thumbs(&st.thumbs, &orphans);
            tracing::info!(
                trashed = res.trashed,
                failed = res.failed,
                companions = res.companions,
                changed = res.changed,
                "deleted rejected photos"
            );
            Ok(res)
        }
    }
}
