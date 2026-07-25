//! Panorama-detection metadata layer: candidate query, temporal clustering, and suggestion-group
//! persistence with incremental-scan markers.
//!
//! This module is **metadata only** — it owns the SQL-prefilter and clustering that narrows the
//! library down to candidate stitch groups, plus the persistence/incrementality plumbing for the
//! background scan. The actual geometric verification lives in `core-pano` (no dependency here).
//!
//! Groups are keyed by [`member_key`] — a blake3 hash of the sorted member content hashes — so a
//! rescan or re-import upserts the *same* row instead of duplicating it, and a user's
//! dismissal/merge decision survives (the upsert never touches `status`/`merged_image_id`). Per-image
//! scan markers keyed on [`ALGO_VERSION`] drive the incremental skip (bump the version to force a
//! full rescan, mirroring the `analysis.rs` staleness pattern).

use std::collections::HashSet;

use core_db::rusqlite::{params, Connection, ToSql, Transaction};
use serde::Serialize;

use crate::error::LibError;

/// Bump to force a library-wide rescan (invalidates every `pano_detect_scan` marker).
pub const ALGO_VERSION: &str = "panodetect-v1";

/// Temporal-clustering thresholds for the metadata prefilter. Generous by design — geometry (in
/// `core-pano`) is the real gate, so the prefilter only needs to avoid pairing shots that plainly
/// can't belong to one sweep.
#[derive(Debug, Clone, Copy)]
pub struct ClusterParams {
    /// Max seconds between consecutive frames before the cluster breaks.
    pub gap_secs: i64,
    /// Max focal-length ratio (max/min) within a cluster before it breaks.
    pub focal_ratio_max: f64,
    /// Drop clusters smaller than this.
    pub min_cluster: usize,
    /// Oversize clusters are split (at their largest internal time gaps) into chunks this size or
    /// smaller.
    pub max_cluster: usize,
}

impl Default for ClusterParams {
    fn default() -> Self {
        Self {
            gap_secs: 30,
            focal_ratio_max: 1.06,
            min_cluster: 2,
            max_cluster: 48,
        }
    }
}

/// One present, dated image considered for panorama grouping.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: i64,
    pub content_hash_hex: String,
    pub format: Option<String>,
    /// Epoch seconds — never NULL for a candidate (the query filters `capture_date NOT NULL`).
    pub capture_date: i64,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub body_serial: Option<String>,
    pub orientation: Option<i64>,
    pub focal_length: Option<f64>,
}

/// All present, dated images ordered so consecutive frames of one sweep sit adjacent: by camera key
/// (`make`, `model`, `serial`), then capture time, then id as a stable tie-break.
pub fn detect_candidates(conn: &Connection) -> Result<Vec<Candidate>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT id, content_hash, format, capture_date, camera_make, camera_model,
                body_serial, orientation, focal_length
         FROM images
         WHERE status = 'present' AND capture_date IS NOT NULL
         ORDER BY camera_make, camera_model, body_serial, capture_date, id",
    )?;
    let rows = stmt.query_map([], |r| {
        let hb: Vec<u8> = r.get(1)?;
        let content_hash_hex = if hb.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&hb);
            core_raw::hex(&a)
        } else {
            String::new()
        };
        Ok(Candidate {
            id: r.get(0)?,
            content_hash_hex,
            format: r.get(2)?,
            capture_date: r.get(3)?,
            camera_make: r.get(4)?,
            camera_model: r.get(5)?,
            body_serial: r.get(6)?,
            orientation: r.get(7)?,
            focal_length: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// True when `b` cannot belong to the same sweep as its predecessor `a`. NULL orientation/focal on
/// either side is treated as compatible (only *both-present-and-different* splits).
fn should_split(a: &Candidate, b: &Candidate, p: &ClusterParams) -> bool {
    // Camera identity change.
    if a.camera_make != b.camera_make
        || a.camera_model != b.camera_model
        || a.body_serial != b.body_serial
    {
        return true;
    }
    // Capture-time gap (candidates are time-ordered within a camera group, so this is >= 0).
    if b.capture_date - a.capture_date > p.gap_secs {
        return true;
    }
    // EXIF orientation flip (only when both are known).
    if let (Some(oa), Some(ob)) = (a.orientation, b.orientation) {
        if oa != ob {
            return true;
        }
    }
    // Focal-length change (only when both are known).
    if let (Some(fa), Some(fb)) = (a.focal_length, b.focal_length) {
        let (lo, hi) = if fa <= fb { (fa, fb) } else { (fb, fa) };
        if lo > 0.0 && hi / lo > p.focal_ratio_max {
            return true;
        }
    }
    false
}

/// Walk the ordered slice, breaking a cluster whenever [`should_split`] fires. Runs shorter than
/// `min_cluster` are dropped; runs longer than `max_cluster` are split at their largest internal
/// capture-time gaps into chunks each `<= max_cluster` (then chunks `< min_cluster` are dropped).
/// Returns index lists into `cands`, in capture order.
pub fn cluster_candidates(cands: &[Candidate], p: &ClusterParams) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    if cands.is_empty() {
        return out;
    }
    let mut run: Vec<usize> = vec![0];
    for i in 1..cands.len() {
        if should_split(&cands[i - 1], &cands[i], p) {
            flush_run(&run, cands, p, &mut out);
            run = vec![i];
        } else {
            run.push(i);
        }
    }
    flush_run(&run, cands, p, &mut out);
    out
}

/// How many images panorama detection still has work for.
///
/// Uses the **same eligibility rule as the scan itself** — `cluster_candidates` drops runs shorter
/// than `min_cluster`, and only frames that survive clustering are ever `mark_scanned`. Counting
/// every dated-but-unmarked image instead would leave lone photos "pending" forever, since nothing
/// will ever mark them.
pub fn pending_count(conn: &Connection, algo: &str) -> Result<i64, LibError> {
    let cands = detect_candidates(conn)?;
    let clusters = cluster_candidates(&cands, &ClusterParams::default());
    let ids: Vec<i64> = clusters
        .iter()
        .flatten()
        .map(|&i| cands[i].id)
        .collect::<Vec<_>>();
    let mut pending = 0i64;
    // Chunked: `images_scanned_at` binds one parameter per id, and a large library would otherwise
    // blow past SQLite's variable limit.
    for chunk in ids.chunks(crate::analysis::SCOPE_CHUNK) {
        let scanned = images_scanned_at(conn, chunk, algo)?;
        pending += chunk.iter().filter(|id| !scanned.contains(id)).count() as i64;
    }
    Ok(pending)
}

/// Emit one raw run: drop if too small, pass through if within bounds, else largest-gap split.
fn flush_run(run: &[usize], cands: &[Candidate], p: &ClusterParams, out: &mut Vec<Vec<usize>>) {
    if run.len() < p.min_cluster {
        return;
    }
    if run.len() <= p.max_cluster {
        out.push(run.to_vec());
        return;
    }
    let mut chunks: Vec<Vec<usize>> = Vec::new();
    split_segment(run, cands, p.max_cluster, &mut chunks);
    for chunk in chunks {
        if chunk.len() >= p.min_cluster {
            out.push(chunk);
        }
    }
}

/// Recursively bisect `seg` at the internal boundary with the largest capture-time gap (earliest on
/// ties), until every chunk is `<= max`. Deterministic; chunks are emitted in capture order.
fn split_segment(seg: &[usize], cands: &[Candidate], max: usize, out: &mut Vec<Vec<usize>>) {
    if seg.len() <= max {
        out.push(seg.to_vec());
        return;
    }
    let mut best_k = 0usize;
    let mut best_gap = i64::MIN;
    for k in 0..seg.len() - 1 {
        let gap = cands[seg[k + 1]].capture_date - cands[seg[k]].capture_date;
        if gap > best_gap {
            best_gap = gap;
            best_k = k;
        }
    }
    // Cut after best_k.
    split_segment(&seg[..=best_k], cands, max, out);
    split_segment(&seg[best_k + 1..], cands, max, out);
}

/// Stable, order-independent key for a group: sort the member content-hash hex strings, decode each
/// to its 32 raw bytes, concatenate, and blake3-hash the result (returned as hex). Permutation of the
/// same set yields the same key; a different set yields a different key.
pub fn member_key(hashes_hex: &[String]) -> String {
    let mut sorted: Vec<&str> = hashes_hex.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut buf: Vec<u8> = Vec::with_capacity(sorted.len() * 32);
    for h in sorted {
        let bytes = h.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&h[i..i + 2], 16) {
                buf.push(b);
            }
            i += 2;
        }
    }
    core_raw::hex(&core_raw::content_hash(&buf))
}

/// Ids among `ids` already scanned at `algo` (the incremental skip-set).
pub fn images_scanned_at(
    conn: &Connection,
    ids: &[i64],
    algo: &str,
) -> Result<HashSet<i64>, LibError> {
    let mut out = HashSet::new();
    if ids.is_empty() {
        return Ok(out);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!(
        "SELECT image_id FROM pano_detect_scan
         WHERE algo_version = ? AND image_id IN ({placeholders})"
    );
    let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(ids.len() + 1);
    binds.push(&algo);
    for id in ids {
        binds.push(id);
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(binds.as_slice(), |r| r.get::<_, i64>(0))?;
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

/// Mark `ids` scanned at `algo`/`now` (INSERT OR REPLACE — the scan marker is per-image, latest wins).
/// Caller owns the transaction.
///
/// Also records the attempt under the shared [`crate::analysis::PANORAMA_STAGE_ID`] stage, in the
/// same transaction, so the per-photo scan readout shows panorama beside the AI stages instead of
/// this table's state being invisible to it. `pano_detect_scan` stays the authoritative skip-set for
/// the scan itself — this is the reporting projection.
pub fn mark_scanned(tx: &Transaction, ids: &[i64], algo: &str, now: i64) -> Result<(), LibError> {
    let mut stmt = tx.prepare(
        "INSERT OR REPLACE INTO pano_detect_scan(image_id, algo_version, scanned_at)
         VALUES (?1, ?2, ?3)",
    )?;
    for &id in ids {
        stmt.execute(params![id, algo, now])?;
    }
    drop(stmt);
    let attempt = [crate::analysis::StageAttempt {
        stage_id: crate::analysis::PANORAMA_STAGE_ID,
        model_version: algo,
        error: None,
    }];
    for &id in ids {
        crate::analysis::record_attempts(tx, id, now, &attempt)?;
    }
    Ok(())
}

/// One verified group to persist: member image ids and their content-hash hexes (both in capture
/// order), plus the group confidence.
pub struct GroupUpsert {
    pub member_ids: Vec<i64>,
    pub member_hashes: Vec<String>,
    pub confidence: f64,
}

/// Replace the suggested groups over one cluster with the freshly `found` set, in a single
/// caller-owned transaction:
///
/// 1. Delete `status='suggested'` groups that intersect `cluster_image_ids` (members cascade) — stale
///    suggestions for this cluster are cleared, but dismissals and merges survive.
/// 2. Upsert each `found` group by `member_key` (`ON CONFLICT DO UPDATE` refreshes confidence /
///    algo / detected_at only — never `status`/`merged_image_id`), then replace its member rows with
///    `position` = capture order.
///
/// Returns the number of `found` groups that are `status='suggested'` after the upsert.
pub fn replace_cluster_groups(
    tx: &Transaction,
    cluster_image_ids: &[i64],
    found: &[GroupUpsert],
    algo: &str,
    now: i64,
) -> Result<usize, LibError> {
    // (1) Drop stale suggestions intersecting this cluster.
    if !cluster_image_ids.is_empty() {
        let placeholders = vec!["?"; cluster_image_ids.len()].join(",");
        let sql = format!(
            "DELETE FROM pano_detect_groups
             WHERE status = 'suggested' AND id IN (
                 SELECT DISTINCT group_id FROM pano_detect_members
                 WHERE image_id IN ({placeholders}))"
        );
        let binds: Vec<&dyn ToSql> = cluster_image_ids
            .iter()
            .map(|id| id as &dyn ToSql)
            .collect();
        tx.execute(&sql, binds.as_slice())?;
    }

    // (2) Upsert each found group + its members.
    let mut suggested = 0usize;
    for g in found {
        let key = member_key(&g.member_hashes);
        tx.execute(
            "INSERT INTO pano_detect_groups (member_key, algo_version, confidence, detected_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(member_key) DO UPDATE SET
                 confidence = excluded.confidence,
                 algo_version = excluded.algo_version,
                 detected_at = excluded.detected_at",
            params![key, algo, g.confidence, now],
        )?;
        let (group_id, status): (i64, String) = tx.query_row(
            "SELECT id, status FROM pano_detect_groups WHERE member_key = ?1",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        tx.execute(
            "DELETE FROM pano_detect_members WHERE group_id = ?1",
            params![group_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO pano_detect_members(group_id, image_id, position) VALUES (?1, ?2, ?3)",
            )?;
            for (pos, &image_id) in g.member_ids.iter().enumerate() {
                stmt.execute(params![group_id, image_id, pos as i64])?;
            }
        }
        if status == "suggested" {
            suggested += 1;
        }
    }
    Ok(suggested)
}

/// One member of a suggestion group (joined to its image), ordered by `position`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanoMemberRow {
    pub image_id: i64,
    pub content_hash: String,
    pub filename: String,
    pub capture_date: Option<i64>,
    pub format: Option<String>,
    pub position: i64,
    /// `false` when the source image's file is missing (`images.status != 'present'` — a soft
    /// delete via `reconcile.rs`, the link row itself survives). Gates the merge handoff in the UI:
    /// `PanoSuggestions` disables "Preview & Merge…" while any member isn't present.
    pub present: bool,
}

/// One suggestion group for the review panel. `all_raw` gates the merge handoff (stitch is RAW-only).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanoGroupRow {
    pub id: i64,
    pub confidence: f64,
    pub status: String,
    pub detected_at: i64,
    pub merged_image_id: Option<i64>,
    pub all_raw: bool,
    pub members: Vec<PanoMemberRow>,
}

/// Suggestion groups newest-first (`detected_at DESC, id DESC`), each with its members in position
/// order. `include_dismissed=false` hides `status='dismissed'`; `merged` groups are always shown (the
/// UI marks them done).
pub fn list_groups(
    conn: &Connection,
    include_dismissed: bool,
) -> Result<Vec<PanoGroupRow>, LibError> {
    let where_status = if include_dismissed {
        ""
    } else {
        "WHERE status != 'dismissed'"
    };
    let sql = format!(
        "SELECT id, confidence, status, detected_at, merged_image_id
         FROM pano_detect_groups
         {where_status}
         ORDER BY detected_at DESC, id DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let group_rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?
        .collect::<core_db::rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(group_rows.len());
    for (id, confidence, status, detected_at, merged_image_id) in group_rows {
        let members = group_members(conn, id)?;
        let all_raw =
            !members.is_empty() && members.iter().all(|m| m.format.as_deref() == Some("raw"));
        out.push(PanoGroupRow {
            id,
            confidence,
            status,
            detected_at,
            merged_image_id,
            all_raw,
            members,
        });
    }
    Ok(out)
}

fn group_members(conn: &Connection, group_id: i64) -> Result<Vec<PanoMemberRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT m.image_id, i.content_hash, i.original_filename, i.capture_date, i.format, m.position,
                i.status
         FROM pano_detect_members m
         JOIN images i ON i.id = m.image_id
         WHERE m.group_id = ?1
         ORDER BY m.position",
    )?;
    let rows = stmt.query_map([group_id], |r| {
        let hb: Vec<u8> = r.get(1)?;
        let content_hash = if hb.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&hb);
            core_raw::hex(&a)
        } else {
            String::new()
        };
        let status: String = r.get(6)?;
        Ok(PanoMemberRow {
            image_id: r.get(0)?,
            content_hash,
            filename: r.get(2)?,
            capture_date: r.get(3)?,
            format: r.get(4)?,
            position: r.get(5)?,
            present: status == "present",
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Count of `status='suggested'` groups (the LeftNav badge / status readout).
pub fn count_suggested(conn: &Connection) -> Result<i64, LibError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM pano_detect_groups WHERE status = 'suggested'",
        [],
        |r| r.get(0),
    )?)
}

/// Set a group's status. Whitelisted to `'suggested'` (undo a dismissal) | `'dismissed'`; use
/// [`set_group_merged`] to record a merge.
pub fn set_group_status(conn: &Connection, group_id: i64, status: &str) -> Result<(), LibError> {
    if status != "suggested" && status != "dismissed" {
        return Err(LibError::Other(format!(
            "invalid pano group status: {status}"
        )));
    }
    conn.execute(
        "UPDATE pano_detect_groups SET status = ?1 WHERE id = ?2",
        params![status, group_id],
    )?;
    Ok(())
}

/// Record that a group was merged into `merged_image_id` (sets `status='merged'`).
pub fn set_group_merged(
    conn: &Connection,
    group_id: i64,
    merged_image_id: i64,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE pano_detect_groups
         SET status = 'merged', merged_image_id = ?2
         WHERE id = ?1",
        params![group_id, merged_image_id],
    )?;
    Ok(())
}

/// Delete `status='suggested'` groups that can never be reviewed again because fewer than 2 of
/// their members still EXIST as catalog rows — a group whose members were hard-deleted (e.g.
/// `core-dedup`'s resolve → migration 019's `ON DELETE CASCADE` takes the `pano_detect_members`
/// rows with them), leaving an unreachable husk that no rescan can clean up: `replace_cluster_groups`
/// only stale-deletes groups intersecting a *re-verified* cluster, and a cluster needs ≥2 present
/// candidates to be emitted at all.
///
/// Deliberately keyed on row EXISTENCE, not `status='present'`. A member that is merely *missing*
/// (`reconcile.rs` soft-deletes when a volume is unmounted) must NOT prune its group: the images
/// come back when the volume does, but the per-image scan markers would suppress re-verification, so
/// pruning here would silently drop the suggestion until a forced rescan. Missing members are
/// surfaced instead — [`PanoMemberRow::present`] dims them in review and blocks the merge.
///
/// Dismissed/merged groups are never touched — pruning clears review-queue noise, never a recorded
/// user decision. Caller owns the transaction; returns the number of groups deleted.
pub fn prune_stale_groups(tx: &Transaction) -> Result<usize, LibError> {
    let n = tx.execute(
        "DELETE FROM pano_detect_groups
         WHERE status = 'suggested' AND id IN (
             SELECT pg.id
             FROM pano_detect_groups pg
             LEFT JOIN pano_detect_members pm ON pm.group_id = pg.id
             LEFT JOIN images i ON i.id = pm.image_id
             WHERE pg.status = 'suggested'
             GROUP BY pg.id
             HAVING SUM(CASE WHEN i.id IS NOT NULL THEN 1 ELSE 0 END) < 2
         )",
        [],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::rusqlite::params;
    use core_db::Db;

    // ---- clustering (pure) ----

    fn cand(
        id: i64,
        capture: i64,
        make: &str,
        model: &str,
        serial: Option<&str>,
        orient: Option<i64>,
        focal: Option<f64>,
    ) -> Candidate {
        Candidate {
            id,
            content_hash_hex: format!("{id:064x}"),
            format: Some("raw".into()),
            capture_date: capture,
            camera_make: Some(make.into()),
            camera_model: Some(model.into()),
            body_serial: serial.map(|s| s.to_string()),
            orientation: orient,
            focal_length: focal,
        }
    }

    #[test]
    fn same_camera_sequence_is_one_cluster() {
        let c = [
            cand(1, 0, "Canon", "R7", None, Some(1), Some(50.0)),
            cand(2, 5, "Canon", "R7", None, Some(1), Some(50.0)),
            cand(3, 10, "Canon", "R7", None, Some(1), Some(50.0)),
        ];
        assert_eq!(
            cluster_candidates(&c, &ClusterParams::default()),
            vec![vec![0, 1, 2]]
        );
    }

    #[test]
    fn time_gap_splits() {
        // gaps 5, 40, 5 → break after index 1 (40 > 30).
        let c = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 5, "Canon", "R7", None, None, None),
            cand(3, 45, "Canon", "R7", None, None, None),
            cand(4, 50, "Canon", "R7", None, None, None),
        ];
        assert_eq!(
            cluster_candidates(&c, &ClusterParams::default()),
            vec![vec![0, 1], vec![2, 3]]
        );
    }

    #[test]
    fn camera_model_change_splits() {
        let c = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 5, "Canon", "R7", None, None, None),
            cand(3, 10, "Canon", "R5", None, None, None),
            cand(4, 15, "Canon", "R5", None, None, None),
        ];
        assert_eq!(
            cluster_candidates(&c, &ClusterParams::default()),
            vec![vec![0, 1], vec![2, 3]]
        );
    }

    #[test]
    fn orientation_change_splits_but_null_is_compatible() {
        // 1 vs 6 splits.
        let split = [
            cand(1, 0, "Canon", "R7", None, Some(1), None),
            cand(2, 5, "Canon", "R7", None, Some(1), None),
            cand(3, 10, "Canon", "R7", None, Some(6), None),
            cand(4, 15, "Canon", "R7", None, Some(6), None),
        ];
        assert_eq!(
            cluster_candidates(&split, &ClusterParams::default()),
            vec![vec![0, 1], vec![2, 3]]
        );
        // NULL orientation on either side never splits.
        let compat = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 5, "Canon", "R7", None, Some(1), None),
            cand(3, 10, "Canon", "R7", None, None, None),
        ];
        assert_eq!(
            cluster_candidates(&compat, &ClusterParams::default()),
            vec![vec![0, 1, 2]]
        );
    }

    #[test]
    fn focal_ratio_splits_but_small_change_and_null_compatible() {
        // 50 → 55: ratio 1.1 > 1.06 → split.
        let split = [
            cand(1, 0, "Canon", "R7", None, None, Some(50.0)),
            cand(2, 5, "Canon", "R7", None, None, Some(50.0)),
            cand(3, 10, "Canon", "R7", None, None, Some(55.0)),
            cand(4, 15, "Canon", "R7", None, None, Some(55.0)),
        ];
        assert_eq!(
            cluster_candidates(&split, &ClusterParams::default()),
            vec![vec![0, 1], vec![2, 3]]
        );
        // 50 → 52: ratio 1.04 <= 1.06 → no split.
        let small = [
            cand(1, 0, "Canon", "R7", None, None, Some(50.0)),
            cand(2, 5, "Canon", "R7", None, None, Some(52.0)),
            cand(3, 10, "Canon", "R7", None, None, Some(50.0)),
        ];
        assert_eq!(
            cluster_candidates(&small, &ClusterParams::default()),
            vec![vec![0, 1, 2]]
        );
        // NULL focal on either side never splits.
        let nullf = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 5, "Canon", "R7", None, None, Some(50.0)),
            cand(3, 10, "Canon", "R7", None, None, None),
        ];
        assert_eq!(
            cluster_candidates(&nullf, &ClusterParams::default()),
            vec![vec![0, 1, 2]]
        );
    }

    #[test]
    fn singleton_cluster_dropped() {
        let c = [cand(1, 0, "Canon", "R7", None, None, None)];
        assert!(cluster_candidates(&c, &ClusterParams::default()).is_empty());
        // A trailing lone frame after a break is dropped too.
        let c2 = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 5, "Canon", "R7", None, None, None),
            cand(3, 100, "Canon", "R7", None, None, None),
        ];
        assert_eq!(
            cluster_candidates(&c2, &ClusterParams::default()),
            vec![vec![0, 1]]
        );
    }

    #[test]
    fn oversize_cluster_splits_at_largest_gaps() {
        // 6 frames within the 30s gap window → one run; max_cluster 3 forces a split at the single
        // largest internal gap (18s, between index 2 and 3).
        let p = ClusterParams {
            gap_secs: 30,
            focal_ratio_max: 1.06,
            min_cluster: 2,
            max_cluster: 3,
        };
        let c = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 1, "Canon", "R7", None, None, None),
            cand(3, 2, "Canon", "R7", None, None, None),
            cand(4, 20, "Canon", "R7", None, None, None),
            cand(5, 21, "Canon", "R7", None, None, None),
            cand(6, 22, "Canon", "R7", None, None, None),
        ];
        let got = cluster_candidates(&c, &p);
        assert_eq!(got, vec![vec![0, 1, 2], vec![3, 4, 5]]);
        for chunk in &got {
            assert!(chunk.len() <= p.max_cluster && chunk.len() >= p.min_cluster);
        }
        // Deterministic across runs.
        assert_eq!(got, cluster_candidates(&c, &p));

        // Recursion: a half still oversize is split again (max_cluster 2 → three pairs).
        let p2 = ClusterParams {
            max_cluster: 2,
            ..p
        };
        let c2 = [
            cand(1, 0, "Canon", "R7", None, None, None),
            cand(2, 1, "Canon", "R7", None, None, None),
            cand(3, 20, "Canon", "R7", None, None, None),
            cand(4, 21, "Canon", "R7", None, None, None),
            cand(5, 40, "Canon", "R7", None, None, None),
            cand(6, 41, "Canon", "R7", None, None, None),
        ];
        let got2 = cluster_candidates(&c2, &p2);
        assert_eq!(got2, vec![vec![0, 1], vec![2, 3], vec![4, 5]]);
        for chunk in &got2 {
            assert!(chunk.len() <= 2 && chunk.len() >= 2);
        }
    }

    // ---- member_key ----

    #[test]
    fn member_key_is_permutation_invariant() {
        let h1 = format!("{:064x}", 0xa1_u64);
        let h2 = format!("{:064x}", 0xb2_u64);
        let h3 = format!("{:064x}", 0xc3_u64);
        let k1 = member_key(&[h1.clone(), h2.clone(), h3.clone()]);
        let k2 = member_key(&[h3.clone(), h1.clone(), h2.clone()]);
        assert_eq!(k1, k2);
        assert_eq!(k1.len(), 64); // blake3 hex
    }

    #[test]
    fn member_key_differs_for_different_sets() {
        let h1 = format!("{:064x}", 0xa1_u64);
        let h2 = format!("{:064x}", 0xb2_u64);
        let h3 = format!("{:064x}", 0xc3_u64);
        assert_ne!(
            member_key(&[h1.clone(), h2.clone()]),
            member_key(&[h1.clone(), h3.clone()])
        );
    }

    // ---- persistence ----

    fn hexhash(tag: u8) -> String {
        core_raw::hex(&[tag; 32])
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_image(
        conn: &Connection,
        tag: u8,
        capture: i64,
        make: &str,
        model: &str,
        orientation: Option<i64>,
        focal: Option<f64>,
        format: &str,
    ) -> i64 {
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                 capture_date, camera_make, camera_model, orientation, focal_length, format, imported_at)
             VALUES (?1, 1, ?2, ?3, 'present', ?4, ?5, ?6, ?7, ?8, ?9, 0)",
            params![
                vec![tag; 32],
                format!("/lib/{tag}.cr3"),
                format!("{tag}.cr3"),
                capture,
                make,
                model,
                orientation,
                focal,
                format,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn group_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM pano_detect_groups", [], |r| r.get(0))
            .unwrap()
    }

    fn member_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM pano_detect_members", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn creates_groups_members_and_computes_all_raw() {
        let mut db = Db::open_in_memory().unwrap();
        // Group A: mixed raw/jpeg. Group B: all raw.
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "jpeg");
        let a3 = insert_image(&db.conn, 3, 110, "Canon", "R7", None, None, "raw");
        let b1 = insert_image(&db.conn, 4, 500, "Canon", "R7", None, None, "raw");
        let b2 = insert_image(&db.conn, 5, 505, "Canon", "R7", None, None, "raw");

        let ga = GroupUpsert {
            member_ids: vec![a1, a2, a3],
            member_hashes: vec![hexhash(1), hexhash(2), hexhash(3)],
            confidence: 2.0,
        };
        let gb = GroupUpsert {
            member_ids: vec![b1, b2],
            member_hashes: vec![hexhash(4), hexhash(5)],
            confidence: 1.5,
        };
        let tx = db.conn.transaction().unwrap();
        let n = replace_cluster_groups(&tx, &[a1, a2, a3, b1, b2], &[ga, gb], ALGO_VERSION, 1000)
            .unwrap();
        tx.commit().unwrap();
        assert_eq!(n, 2, "both freshly-inserted groups are suggested");

        assert_eq!(group_count(&db.conn), 2);
        assert_eq!(member_count(&db.conn), 5);
        assert_eq!(count_suggested(&db.conn).unwrap(), 2);

        let groups = list_groups(&db.conn, false).unwrap();
        assert_eq!(groups.len(), 2);
        let group_a = groups.iter().find(|g| g.members.len() == 3).unwrap();
        let group_b = groups.iter().find(|g| g.members.len() == 2).unwrap();
        // Members ordered by position (capture order).
        assert_eq!(
            group_a
                .members
                .iter()
                .map(|m| m.image_id)
                .collect::<Vec<_>>(),
            vec![a1, a2, a3]
        );
        assert_eq!(group_a.members[0].content_hash, hexhash(1));
        assert!(!group_a.all_raw, "mixed jpeg/raw group is not all_raw");
        assert!(group_b.all_raw, "all-raw group is all_raw");
    }

    #[test]
    fn rerun_with_same_found_is_idempotent() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let mk = || GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        for now in [1000, 2000] {
            let tx = db.conn.transaction().unwrap();
            replace_cluster_groups(&tx, &[a1, a2], &[mk()], ALGO_VERSION, now).unwrap();
            tx.commit().unwrap();
        }
        // Upsert by member_key → one group row, not two.
        assert_eq!(group_count(&db.conn), 1);
        assert_eq!(member_count(&db.conn), 2);
        // detected_at refreshed by the second run.
        let detected_at: i64 = db
            .conn
            .query_row("SELECT detected_at FROM pano_detect_groups", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(detected_at, 2000);
    }

    #[test]
    fn dismissal_survives_rerun_and_stale_suggestions_are_replaced() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let a3 = insert_image(&db.conn, 3, 110, "Canon", "R7", None, None, "raw");
        // Unrelated group on a disjoint cluster — must never be touched by the A-cluster re-run.
        let b1 = insert_image(&db.conn, 4, 500, "Canon", "R7", None, None, "raw");
        let b2 = insert_image(&db.conn, 5, 505, "Canon", "R7", None, None, "raw");

        let ga = || GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        // gc is a second suggestion over the A cluster (members a1,a3) that will NOT be re-found.
        let gc = GroupUpsert {
            member_ids: vec![a1, a3],
            member_hashes: vec![hexhash(1), hexhash(3)],
            confidence: 0.9,
        };
        let gb = GroupUpsert {
            member_ids: vec![b1, b2],
            member_hashes: vec![hexhash(4), hexhash(5)],
            confidence: 1.0,
        };

        // Seed the unrelated B group and the two A-cluster suggestions.
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[b1, b2], &[gb], ALGO_VERSION, 1000).unwrap();
        replace_cluster_groups(&tx, &[a1, a2, a3], &[ga(), gc], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();
        assert_eq!(group_count(&db.conn), 3);

        // Dismiss group A.
        let ga_id: i64 = db
            .conn
            .query_row(
                "SELECT id FROM pano_detect_groups WHERE member_key = ?1",
                params![member_key(&[hexhash(1), hexhash(2)])],
                |r| r.get(0),
            )
            .unwrap();
        set_group_status(&db.conn, ga_id, "dismissed").unwrap();
        assert_eq!(count_suggested(&db.conn).unwrap(), 2); // gc + gb

        // Re-run the A cluster finding ONLY group A again.
        let tx = db.conn.transaction().unwrap();
        let n = replace_cluster_groups(&tx, &[a1, a2, a3], &[ga()], ALGO_VERSION, 2000).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            n, 0,
            "re-found group A stays dismissed, not counted as suggested"
        );

        // A survived as dismissed; gc (suggested, intersecting, not re-found) was deleted; B untouched.
        let status_a: String = db
            .conn
            .query_row(
                "SELECT status FROM pano_detect_groups WHERE id = ?1",
                params![ga_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status_a, "dismissed");
        let gc_gone: bool = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pano_detect_groups WHERE member_key = ?1",
                params![member_key(&[hexhash(1), hexhash(3)])],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 0;
        assert!(
            gc_gone,
            "stale suggested group not re-found must be deleted"
        );
        let b_present: bool = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pano_detect_groups WHERE member_key = ?1",
                params![member_key(&[hexhash(4), hexhash(5)])],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1;
        assert!(b_present, "disjoint group must be untouched");

        // include_dismissed toggles visibility of A; B always visible.
        assert_eq!(list_groups(&db.conn, false).unwrap().len(), 1); // only B (gc gone, A dismissed)
        assert_eq!(list_groups(&db.conn, true).unwrap().len(), 2); // A + B
    }

    #[test]
    fn set_group_merged_records_status_and_id() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let merged = insert_image(&db.conn, 9, 105, "Canon", "R7", None, None, "raw");
        let ga = GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[a1, a2], &[ga], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();
        let gid: i64 = db
            .conn
            .query_row("SELECT id FROM pano_detect_groups", [], |r| r.get(0))
            .unwrap();

        set_group_merged(&db.conn, gid, merged).unwrap();
        let groups = list_groups(&db.conn, false).unwrap();
        assert_eq!(groups.len(), 1, "merged group is always listed");
        assert_eq!(groups[0].status, "merged");
        assert_eq!(groups[0].merged_image_id, Some(merged));
        assert_eq!(count_suggested(&db.conn).unwrap(), 0);
    }

    // ---- prune_stale_groups / present ----

    fn set_missing(conn: &Connection, image_id: i64) {
        conn.execute(
            "UPDATE images SET status = 'missing' WHERE id = ?1",
            params![image_id],
        )
        .unwrap();
    }

    /// The zombie case prune exists for: members hard-deleted (dedup resolve) cascade their
    /// `pano_detect_members` rows away, leaving a husk no rescan can reach.
    #[test]
    fn prune_removes_suggested_group_whose_members_were_deleted() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let ga = GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[a1, a2], &[ga], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();
        assert_eq!(group_count(&db.conn), 1);

        // a1 is hard-deleted (dedup resolve → ON DELETE CASCADE drops its member row).
        db.conn
            .execute("DELETE FROM images WHERE id = ?1", params![a1])
            .unwrap();

        let tx = db.conn.transaction().unwrap();
        let pruned = prune_stale_groups(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(pruned, 1);
        assert_eq!(
            group_count(&db.conn),
            0,
            "group with <2 surviving member rows must be pruned"
        );
    }

    /// A merely-*missing* member (unmounted volume) must NOT prune: the images come back, but the
    /// per-image scan markers would suppress re-verification, so the suggestion would be lost until
    /// a forced rescan. Review UI dims such members instead.
    #[test]
    fn prune_keeps_group_whose_member_is_only_missing() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let ga = GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[a1, a2], &[ga], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();

        set_missing(&db.conn, a1);

        let tx = db.conn.transaction().unwrap();
        let pruned = prune_stale_groups(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(pruned, 0, "a soft-deleted member must not drop the group");
        assert_eq!(group_count(&db.conn), 1);
        // …and the member is reported as absent so review can dim it + block the merge.
        let groups = list_groups(&db.conn, false).unwrap();
        let m1 = groups[0].members.iter().find(|m| m.image_id == a1).unwrap();
        assert!(!m1.present);
    }

    #[test]
    fn prune_leaves_dismissed_group_alone() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let ga = GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[a1, a2], &[ga], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();
        let gid: i64 = db
            .conn
            .query_row("SELECT id FROM pano_detect_groups", [], |r| r.get(0))
            .unwrap();
        set_group_status(&db.conn, gid, "dismissed").unwrap();

        // Both members go missing — would qualify for pruning if the group weren't dismissed.
        set_missing(&db.conn, a1);
        set_missing(&db.conn, a2);

        let tx = db.conn.transaction().unwrap();
        let pruned = prune_stale_groups(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(
            pruned, 0,
            "prune only ever touches status='suggested' groups"
        );
        assert_eq!(group_count(&db.conn), 1);
        let status: String = db
            .conn
            .query_row(
                "SELECT status FROM pano_detect_groups WHERE id = ?1",
                params![gid],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(status, "dismissed");
    }

    #[test]
    fn prune_leaves_healthy_group_alone() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let ga = GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[a1, a2], &[ga], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();

        let tx = db.conn.transaction().unwrap();
        let pruned = prune_stale_groups(&tx).unwrap();
        tx.commit().unwrap();
        assert_eq!(pruned, 0);
        assert_eq!(
            group_count(&db.conn),
            1,
            "healthy 2-present-member group survives"
        );
    }

    #[test]
    fn list_groups_reports_present_false_for_missing_member() {
        let mut db = Db::open_in_memory().unwrap();
        let a1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let a2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let ga = GroupUpsert {
            member_ids: vec![a1, a2],
            member_hashes: vec![hexhash(1), hexhash(2)],
            confidence: 1.0,
        };
        let tx = db.conn.transaction().unwrap();
        replace_cluster_groups(&tx, &[a1, a2], &[ga], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();

        set_missing(&db.conn, a2);

        let groups = list_groups(&db.conn, false).unwrap();
        assert_eq!(groups.len(), 1);
        let m1 = groups[0].members.iter().find(|m| m.image_id == a1).unwrap();
        let m2 = groups[0].members.iter().find(|m| m.image_id == a2).unwrap();
        assert!(m1.present, "present image must report present=true");
        assert!(!m2.present, "missing image must report present=false");
    }

    #[test]
    fn set_group_status_rejects_invalid() {
        let db = Db::open_in_memory().unwrap();
        assert!(set_group_status(&db.conn, 1, "merged").is_err());
        assert!(set_group_status(&db.conn, 1, "bogus").is_err());
        assert!(set_group_status(&db.conn, 1, "suggested").is_ok());
        assert!(set_group_status(&db.conn, 1, "dismissed").is_ok());
    }

    #[test]
    fn scan_markers_round_trip_keyed_on_algo() {
        let mut db = Db::open_in_memory().unwrap();
        let i1 = insert_image(&db.conn, 1, 100, "Canon", "R7", None, None, "raw");
        let i2 = insert_image(&db.conn, 2, 105, "Canon", "R7", None, None, "raw");
        let i3 = insert_image(&db.conn, 3, 110, "Canon", "R7", None, None, "raw");

        let tx = db.conn.transaction().unwrap();
        mark_scanned(&tx, &[i1, i2], ALGO_VERSION, 1000).unwrap();
        tx.commit().unwrap();

        let scanned = images_scanned_at(&db.conn, &[i1, i2, i3], ALGO_VERSION).unwrap();
        assert_eq!(scanned, HashSet::from([i1, i2]));
        // A different algo version sees nothing (drives full rescan on version bump).
        let other = images_scanned_at(&db.conn, &[i1, i2, i3], "panodetect-v2").unwrap();
        assert!(other.is_empty());
        // Empty id list short-circuits.
        assert!(images_scanned_at(&db.conn, &[], ALGO_VERSION)
            .unwrap()
            .is_empty());
    }
}
