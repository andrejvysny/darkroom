//! Panorama-detection background job: whole-library incremental scan that pairs
//! `core_library::pano_detect`'s metadata prefilter/clustering/persistence with
//! `core_pano::detect_groups`'s geometric verification over cached 512px thumbs.
//!
//! Mirrors `panorama.rs`'s `RunGuard` + brief-DB-lock discipline (the ea0d66a import-freeze
//! lesson: heavy work — thumb decode and `detect_groups` — never runs under the DB lock, only the
//! candidate query, the scan-marker check, and the final upsert transaction do) and `analysis.rs`'s
//! per-batch progress-event / cancel-drains-to-`done` convention (cancellation ends the scan
//! early but still reports whatever was found so far — it is not treated as an error).
//!
//! Unconditional module: no ML dependency, so unlike `analysis`/`faces` it needs no cfg-gated stub.

use std::sync::atomic::{AtomicBool, Ordering};

use core_library::pano_detect::{
    cluster_candidates, count_suggested, detect_candidates, images_scanned_at, list_groups,
    mark_scanned, prune_stale_groups, replace_cluster_groups, set_group_merged, set_group_status,
    Candidate, ClusterParams, GroupUpsert, PanoGroupRow, ALGO_VERSION,
};
use core_pano::{detect_groups, DetectOptions, Frame};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::state::AppState;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanoDetectStatus {
    pub running: bool,
    pub suggested: i64,
}

/// Running flag + the current `suggested`-group count (LeftNav badge / status readout).
pub fn status(st: &AppState) -> Result<PanoDetectStatus, String> {
    let running = st.pano_detect_running.load(Ordering::SeqCst);
    let db = st.db.lock().map_err(|e| e.to_string())?;
    let suggested = count_suggested(&db.conn).map_err(|e| e.to_string())?;
    Ok(PanoDetectStatus { running, suggested })
}

/// Request the running scan to stop. No-op if idle.
pub fn cancel(st: &AppState) {
    if st.pano_detect_running.load(Ordering::SeqCst) {
        st.pano_detect_cancel.store(true, Ordering::SeqCst);
    }
}

/// Resets `pano_detect_running`/`pano_detect_cancel` on drop so an early return / error can never
/// wedge the job gate.
struct RunGuard<'a> {
    running: &'a AtomicBool,
    cancel: &'a AtomicBool,
}
impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        self.cancel.store(false, Ordering::SeqCst);
    }
}

fn emit_progress<R: Runtime>(app: &AppHandle<R>, phase: &str, done: usize, total: usize) {
    let _ = app.emit(
        "pano_detect:progress",
        serde_json::json!({ "phase": phase, "done": done, "total": total }),
    );
}

/// Decode a cluster member's cached 512px thumb into a `core_pano::Frame`. Returns `None` if the
/// thumb is missing or fails to decode — the caller skips it (dedup's similarity-backfill precedent
/// for treating an unreadable/missing thumb as skip-not-fail).
fn load_frame(st: &AppState, c: &Candidate) -> Option<Frame> {
    let bytes = st
        .thumbs
        .read(&c.content_hash_hex, core_library::THUMB_SIZE)
        .ok()?;
    let img = image::load_from_memory(&bytes).ok()?;
    let rgb = img.to_rgb8();
    let (width, height) = (rgb.width() as usize, rgb.height() as usize);
    let data: Vec<f32> = rgb
        .into_raw()
        .into_iter()
        .map(|b| b as f32 / 255.0)
        .collect();
    Some(Frame {
        width,
        height,
        rgb: data,
        focal_seed_px: None,
    })
}

/// Run (or resume) the whole-library panorama-detection scan: metadata clustering under a brief DB
/// lock, then per-cluster geometric verification unlocked, then a brief upsert transaction. `force`
/// bypasses the per-image `pano_detect_scan` markers and re-verifies every cluster. Returns the
/// accumulated count of freshly-`suggested` groups; also emitted on `pano_detect:done`.
pub fn run<R: Runtime>(app: &AppHandle<R>, force: bool) -> Result<usize, String> {
    let st = app.state::<AppState>();
    if st.pano_detect_running.swap(true, Ordering::SeqCst) {
        return Err("a panorama detection scan is already running".into());
    }
    st.pano_detect_cancel.store(false, Ordering::SeqCst);
    let _guard = RunGuard {
        running: &st.pano_detect_running,
        cancel: &st.pano_detect_cancel,
    };
    let cancelled = || st.pano_detect_cancel.load(Ordering::SeqCst);

    // Brief lock: prune zombie suggestions (members gone missing, or a group whose member rows
    // fully cascaded away) before scanning — keeps the review queue honest even for a cluster this
    // pass never re-touches.
    {
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.transaction().map_err(|e| e.to_string())?;
        let pruned = prune_stale_groups(&tx).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        if pruned > 0 {
            tracing::debug!(pruned, "pano_detect: pruned stale suggestion groups");
        }
    }

    // Brief lock: candidate query + pure clustering.
    let clusters: Vec<Vec<Candidate>> = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let cands = detect_candidates(&db.conn).map_err(|e| e.to_string())?;
        cluster_candidates(&cands, &ClusterParams::default())
            .into_iter()
            .map(|idxs| idxs.into_iter().map(|i| cands[i].clone()).collect())
            .collect()
    };
    let total = clusters.len();
    emit_progress(app, "cluster", 0, total);

    let mut found_total: usize = 0;

    for (idx, cluster) in clusters.into_iter().enumerate() {
        if cancelled() {
            break;
        }

        let cluster_ids: Vec<i64> = cluster.iter().map(|c| c.id).collect();

        if !force {
            let scanned = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                images_scanned_at(&db.conn, &cluster_ids, ALGO_VERSION)
                    .map_err(|e| e.to_string())?
            };
            if cluster_ids.iter().all(|id| scanned.contains(id)) {
                emit_progress(app, "verify", idx + 1, total);
                continue;
            }
        }

        // Unlocked: load cached thumbs + decode. Track which candidate survives at which frame
        // index — `DetectedGroup::members` indexes the surviving-frames vec, not the cluster.
        let mut frames: Vec<Frame> = Vec::with_capacity(cluster.len());
        let mut frame_candidates: Vec<&Candidate> = Vec::with_capacity(cluster.len());
        for c in &cluster {
            if let Some(frame) = load_frame(&st, c) {
                frames.push(frame);
                frame_candidates.push(c);
            }
        }

        if frames.len() < 2 {
            // Too few decodable members to verify anything — mark the whole cluster scanned (incl.
            // the skipped-thumb members) so the scan terminates; `force` covers thumbs that later
            // appear.
            let now = core_library::now_epoch();
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            mark_scanned(&tx, &cluster_ids, ALGO_VERSION, now).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            emit_progress(app, "verify", idx + 1, total);
            continue;
        }

        let report = detect_groups(&frames, None, &DetectOptions::default(), &cancelled);
        if cancelled() {
            // Cancelled mid-cluster: drop the partial report, don't persist this cluster.
            break;
        }

        let upserts: Vec<GroupUpsert> = report
            .groups
            .iter()
            .map(|g| {
                let member_ids: Vec<i64> = g
                    .members
                    .iter()
                    .map(|&fi| frame_candidates[fi].id)
                    .collect();
                let member_hashes: Vec<String> = g
                    .members
                    .iter()
                    .map(|&fi| frame_candidates[fi].content_hash_hex.clone())
                    .collect();
                GroupUpsert {
                    member_ids,
                    member_hashes,
                    confidence: g.confidence,
                }
            })
            .collect();

        let now = core_library::now_epoch();
        {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            let suggested = replace_cluster_groups(&tx, &cluster_ids, &upserts, ALGO_VERSION, now)
                .map_err(|e| e.to_string())?;
            // Skipped-thumb members are deliberately marked scanned too, alongside the verified
            // ones — see the comment above.
            mark_scanned(&tx, &cluster_ids, ALGO_VERSION, now).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            found_total += suggested;
        }

        emit_progress(app, "verify", idx + 1, total);
    }

    // Cancellation ends the scan early but is not an error — report whatever was found so far
    // (mirrors `analysis.rs::run_pass`, which emits `analysis:done` with partial stats on cancel).
    let _ = app.emit(
        "pano_detect:done",
        serde_json::json!({ "found": found_total }),
    );
    Ok(found_total)
}

/// Suggestion groups for the review panel (newest-first, each with its members in capture order).
pub fn groups(st: &AppState, include_dismissed: bool) -> Result<Vec<PanoGroupRow>, String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    list_groups(&db.conn, include_dismissed).map_err(|e| e.to_string())
}

/// Dismiss (or undo a dismissal of) a suggestion group.
pub fn dismiss(st: &AppState, group_id: i64, dismissed: bool) -> Result<(), String> {
    let status = if dismissed { "dismissed" } else { "suggested" };
    let db = st.db.lock().map_err(|e| e.to_string())?;
    set_group_status(&db.conn, group_id, status).map_err(|e| e.to_string())
}

/// Record that a group was merged into `merged_image_id` (the merge-flow handoff).
pub fn mark_merged(st: &AppState, group_id: i64, merged_image_id: i64) -> Result<(), String> {
    let db = st.db.lock().map_err(|e| e.to_string())?;
    set_group_merged(&db.conn, group_id, merged_image_id).map_err(|e| e.to_string())
}
