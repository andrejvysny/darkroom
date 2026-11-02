//! Dedup half of the trash flow: `dedup_resolve` and `dedup_resolve_bulk` run the same
//! plan → unlocked IO → fenced publish protocol as the reject half in [`super`].

use super::{orphan_thumb_hashes, refresh_planned_sources, remove_orphan_thumbs, Publish, CHANGED};
use crate::state::AppState;
use core_db::rusqlite::Connection;
use core_db::Db;
use core_model::identity::{CatalogEpoch, ImageId};
use core_runtime::publication::PublicationGate;
use std::path::Path;
use std::sync::Mutex;

// ---------- dedup_resolve ----------

/// A planned duplicate-resolve run: the epoch it was derived on plus the victim snapshot.
pub(crate) struct ResolveRun {
    pub epoch: CatalogEpoch,
    pub plan: core_dedup::ResolvePlan,
}

/// Plan phase under a brief lock: validate the keeper, snapshot the victims, retire their
/// publication identities.
pub(crate) fn plan_resolve(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    keep_id: i64,
    trash_ids: &[i64],
) -> Result<ResolveRun, String> {
    let epoch = publications.epoch();
    let mut gate = publications
        .commit_guard(epoch)
        .map_err(|_| CHANGED.to_string())?;
    let plan = {
        let db = db.lock().map_err(|e| e.to_string())?;
        core_dedup::plan_resolve(&db.conn, keep_id, trash_ids).map_err(|e| e.to_string())?
    };
    for v in &plan.victims {
        gate.retire(ImageId(v.id));
    }
    drop(gate);
    Ok(ResolveRun { epoch, plan })
}

/// Work phase, no locks held: trash each planned victim (+ sidecar), re-checking the keeper's file
/// before every move; an aborted run reports `keeper_lost`.
pub(crate) fn trash_resolve(
    plan: &core_dedup::ResolvePlan,
    remove: &dyn Fn(&Path) -> bool,
) -> core_dedup::TrashedVictims {
    core_dedup::trash_victims(plan, remove)
}

/// Publish phase for one group: fenced finalize + per-source re-sync + orphan hashes + events.
pub(crate) fn publish_resolve(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    run: &ResolveRun,
    trashed: &core_dedup::TrashedVictims,
    log: &dyn Fn(&Connection),
) -> Result<Publish<core_dedup::ResolveResult>, String> {
    let mut gate = match publications.commit_guard(run.epoch) {
        Ok(gate) => gate,
        Err(core_runtime::publication::Stale) => return Ok(Publish::Stale),
    };
    let db = db.lock().map_err(|e| e.to_string())?;
    let res = core_dedup::finalize_resolve(&db.conn, trashed).map_err(|e| e.to_string())?;
    // Per-source, not `invalidate_all`: the catalog was mutated, not replaced (see module docs).
    // Covers every planned victim, not just `gone`: failed/changed/keeper-lost rows were left in
    // the catalog by finalize but the plan retired their identities.
    refresh_planned_sources(&mut gate, &db.conn, run.plan.victims.iter().map(|v| v.id))?;
    let orphans = orphan_thumb_hashes(&db.conn, &res.trashed_hashes);
    log(&db.conn);
    drop(db);
    drop(gate);
    Ok(Publish::Published(res, orphans))
}

/// Behavioural log: keeper choice + full candidate set + (if known) the auto-suggested keeper, so
/// keeper ranking can be learned later and user overrides detected.
#[allow(clippy::too_many_arguments)]
fn log_resolve_events(
    st: &AppState,
    conn: &Connection,
    keep_id: i64,
    trash_ids: &[i64],
    candidate_ids: Option<&[i64]>,
    auto_keeper_id: Option<i64>,
    group_id: Option<&str>,
) {
    let candidates: Vec<i64> = candidate_ids.map(<[i64]>::to_vec).unwrap_or_else(|| {
        let mut v = trash_ids.to_vec();
        v.push(keep_id);
        v.sort_unstable();
        v
    });
    let gid = group_id
        .map(str::to_string)
        .or_else(|| candidates.iter().min().map(|m| format!("dedup-{m}")));
    let cands_json = core_library::ids_json(&candidates);
    let ev = |event_type: &str| {
        crate::events::stamp(
            st,
            core_library::Event {
                event_type: event_type.into(),
                group_id: gid.clone(),
                candidate_ids: Some(cands_json.clone()),
                suggestion_id: auto_keeper_id,
                ..Default::default()
            },
        )
    };
    let _ = core_library::append_event(conn, &ev("dedup.group_shown"));
    let _ = core_library::append_event(
        conn,
        &core_library::Event {
            chosen_id: Some(keep_id),
            rejected_ids: Some(core_library::ids_json(trash_ids)),
            ..ev("dedup.keeper_chosen")
        },
    );
    if auto_keeper_id.is_some_and(|ak| ak != keep_id) {
        let _ = core_library::append_event(
            conn,
            &core_library::Event {
                chosen_id: Some(keep_id),
                ..ev("dedup.override")
            },
        );
    }
}

/// Full `dedup_resolve` run: plan → unlock → trash → fenced publish. Returns files trashed.
///
/// Serialized against flag/pair mutations by the job queue, not a lock: `cull_set_flag{,_many}`,
/// `image_pair_unlink` and this command all run as `JobClass::UserCommand` jobs with
/// `QueuePolicy::Ordered` under the same owner, and the worker's candidate filter
/// (`crates/core-runtime/src/jobs/worker.rs`) never starts a second Running Ordered UserCommand
/// job for that owner. That is what keeps a victim from being un-rejected or re-paired between
/// this run's plan and finalize — the epoch fence catches resets, not culling edits.
pub(crate) fn run_resolve(
    st: &AppState,
    keep_id: i64,
    trash_ids: &[i64],
    candidate_ids: Option<Vec<i64>>,
    auto_keeper_id: Option<i64>,
    group_id: Option<String>,
) -> Result<usize, String> {
    let run = plan_resolve(&st.publications, &st.db, keep_id, trash_ids)?;
    // Documented precondition of `clear_source_caches` (state.rs): the publication gate must be
    // held so reused row ids cannot retain another source's caches.
    st.publications.with_mutation(|| st.clear_source_caches());
    let ctx = core_dedup::make_trash_ctx();
    let trashed = trash_resolve(&run.plan, &|p| ctx.delete(p).is_ok());
    let published = publish_resolve(&st.publications, &st.db, &run, &trashed, &|conn| {
        log_resolve_events(
            st,
            conn,
            keep_id,
            trash_ids,
            candidate_ids.as_deref(),
            auto_keeper_id,
            group_id.as_deref(),
        )
    })?;
    match published {
        Publish::Stale => {
            tracing::warn!(
                victims_trashed = trashed.gone.len(),
                failed = trashed.failed,
                "catalog was reset while duplicate victims were being trashed; \
                 the new catalog is left untouched"
            );
            Ok(0)
        }
        Publish::Published(res, orphans) => {
            st.publications.with_mutation(|| st.clear_source_caches());
            remove_orphan_thumbs(&st.thumbs, &orphans);
            tracing::info!(
                trashed = res.trashed,
                failed = res.failed,
                changed = res.changed,
                keeper_lost = res.keeper_lost,
                "resolved duplicate group"
            );
            Ok(res.trashed)
        }
    }
}

// ---------- dedup_resolve_bulk ----------

/// A planned bulk auto-resolve: every byte-identical group's plan on one epoch.
pub(crate) struct BulkRun {
    pub epoch: CatalogEpoch,
    pub plans: Vec<core_dedup::ResolvePlan>,
}

/// Plan phase: every byte-identical group in one brief lock, retiring all victims' identities.
pub(crate) fn plan_bulk(publications: &PublicationGate, db: &Mutex<Db>) -> Result<BulkRun, String> {
    let epoch = publications.epoch();
    let mut gate = publications
        .commit_guard(epoch)
        .map_err(|_| CHANGED.to_string())?;
    let plans = {
        let db = db.lock().map_err(|e| e.to_string())?;
        core_dedup::plan_auto_resolve_byte_identical(&db.conn).map_err(|e| e.to_string())?
    };
    for plan in &plans {
        for v in &plan.victims {
            gate.retire(ImageId(v.id));
        }
    }
    drop(gate);
    Ok(BulkRun { epoch, plans })
}

/// Work phase, no locks held: trash every plan's victims; keeper loss aborts one plan only.
pub(crate) fn trash_bulk(
    plans: &[core_dedup::ResolvePlan],
    remove: &dyn Fn(&Path) -> bool,
) -> Vec<core_dedup::TrashedVictims> {
    plans
        .iter()
        .map(|p| core_dedup::trash_victims(p, remove))
        .collect()
}

/// Publish phase: one lock, `finalize_resolve` per plan, aggregate; per-source re-sync of every
/// planned victim + orphan hashes at the actual catalog mutation.
pub(crate) fn publish_bulk(
    publications: &PublicationGate,
    db: &Mutex<Db>,
    epoch: CatalogEpoch,
    plans: &[core_dedup::ResolvePlan],
    trashed: &[core_dedup::TrashedVictims],
) -> Result<Publish<core_dedup::ResolveResult>, String> {
    let mut gate = match publications.commit_guard(epoch) {
        Ok(gate) => gate,
        Err(core_runtime::publication::Stale) => return Ok(Publish::Stale),
    };
    let db = db.lock().map_err(|e| e.to_string())?;
    let mut out = core_dedup::ResolveResult::default();
    for t in trashed {
        let r = core_dedup::finalize_resolve(&db.conn, t).map_err(|e| e.to_string())?;
        out.trashed += r.trashed;
        out.failed += r.failed;
        out.changed += r.changed;
        out.keeper_lost += r.keeper_lost;
        out.trashed_hashes.extend(r.trashed_hashes);
    }
    // Per-source, not `invalidate_all`: the catalog was mutated, not replaced (see module docs).
    // Every planned victim — not just the removed ones — is re-checked so kept rows become
    // publishable again.
    refresh_planned_sources(
        &mut gate,
        &db.conn,
        plans.iter().flat_map(|p| p.victims.iter().map(|v| v.id)),
    )?;
    let orphans = orphan_thumb_hashes(&db.conn, &out.trashed_hashes);
    drop(db);
    drop(gate);
    Ok(Publish::Published(out, orphans))
}

/// Full `dedup_resolve_bulk` run: one plan lock, unlocked IO per group, one finalize lock.
/// Returns files trashed.
pub(crate) fn run_resolve_bulk(st: &AppState) -> Result<usize, String> {
    let run = plan_bulk(&st.publications, &st.db)?;
    // Documented precondition of `clear_source_caches` (state.rs): the publication gate must be
    // held so reused row ids cannot retain another source's caches.
    st.publications.with_mutation(|| st.clear_source_caches());
    let ctx = core_dedup::make_trash_ctx();
    let trashed = trash_bulk(&run.plans, &|p| ctx.delete(p).is_ok());
    let published = publish_bulk(&st.publications, &st.db, run.epoch, &run.plans, &trashed)?;
    match published {
        Publish::Stale => {
            tracing::warn!(
                victims_trashed = trashed.iter().map(|t| t.gone.len()).sum::<usize>(),
                failed = trashed.iter().map(|t| t.failed).sum::<usize>(),
                "catalog was reset while duplicate victims were being trashed; \
                 the new catalog is left untouched"
            );
            Ok(0)
        }
        Publish::Published(res, orphans) => {
            st.publications.with_mutation(|| st.clear_source_caches());
            remove_orphan_thumbs(&st.thumbs, &orphans);
            tracing::info!(
                groups = run.plans.len(),
                trashed = res.trashed,
                failed = res.failed,
                changed = res.changed,
                keeper_lost = res.keeper_lost,
                "auto-resolved byte-identical duplicates"
            );
            Ok(res.trashed)
        }
    }
}
