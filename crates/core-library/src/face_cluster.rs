//! Incremental face clustering — assign new faces to people without re-clustering existing ones.
//!
//! The Immich pattern: each unassigned face looks for nearest neighbors (brute-force cosine over the
//! L2-normalized embeddings — no vector DB; fine to ~100k faces). If enough neighbors already belong
//! to one person it joins them; otherwise, if enough *unassigned* faces mutually cluster, they seed a
//! new (unnamed) person; otherwise the face is deferred until more neighbors arrive. Confirmed and
//! rejected faces are sticky — never auto-reassigned — and `face_rejection` pairs are never re-suggested.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use core_db::rusqlite::{params, Connection};
use serde::Serialize;

use crate::error::LibError;
use crate::face::{create_person, faces_for_clustering, prune_empty_unnamed, rejection_pairs};

/// Clustering thresholds. Cosine distance on L2-normalized embeddings (same person ≈ 0.2, different
/// ≈ 0.95 for ArcFace — validated). Defaults are deliberately strict to avoid false merges; override
/// for calibration via `DARKROOM_FACE_MAX_DIST` / `DARKROOM_FACE_JOIN_MIN` / `DARKROOM_FACE_NEW_MIN`.
#[derive(Debug, Clone, Copy)]
pub struct ClusterParams {
    pub max_distance: f32,
    /// Min neighbors of an existing person within `max_distance` to join it.
    pub join_min: usize,
    /// Min mutually-near unassigned faces (incl. the seed) to form a new cluster.
    pub new_min: usize,
}

impl Default for ClusterParams {
    fn default() -> Self {
        Self {
            max_distance: env_f32("DARKROOM_FACE_MAX_DIST", 0.45),
            join_min: env_usize("DARKROOM_FACE_JOIN_MIN", 2),
            new_min: env_usize("DARKROOM_FACE_NEW_MIN", 3),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterStats {
    pub assigned: usize,
    pub new_people: usize,
    pub deferred: usize,
}

/// Candidate faces re-checked between cancel polls during a long bulk cluster, so a multi-hour scan's
/// clustering phase stays responsive. Partial work commits (assignments are independent + idempotent;
/// unprocessed dirty faces stay unassigned and resume next pass).
const CANCEL_CHECK_EVERY: usize = 256;

fn cosine_dist(a: &[f32], b: &[f32]) -> f32 {
    // Both L2-normalized ⇒ cosine distance = 1 − dot. Callers only pass equal-length vectors (the
    // dim guard in `cluster_assign` excludes mismatched ones), so the zip covers the full vector.
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

/// True if any face at `model_tag` still needs clustering (unassigned + non-sticky). Lets the caller
/// skip the whole pass when nothing changed — clustering is otherwise re-run on every scan.
pub fn has_dirty_faces(conn: &Connection, model_tag: &str) -> Result<bool, LibError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM face f JOIN face_embedding e ON e.face_id = f.id
          WHERE e.model_tag = ?1 AND f.person_id IS NULL
            AND f.status NOT IN ('rejected','ignored')",
        params![model_tag],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Run one incremental clustering pass over every embedded face at `model_tag`. Assigned/confirmed
/// faces are left in place; only unassigned (`person_id IS NULL`) non-rejected faces are placed.
/// `cancel` is polled periodically — on cancellation the work done so far is committed and the pass
/// returns early (remaining dirty faces resume on the next run).
pub fn cluster_assign(
    conn: &mut Connection,
    model_tag: &str,
    now: i64,
    p: ClusterParams,
    cancel: &AtomicBool,
) -> Result<ClusterStats, LibError> {
    // Quality-sorted so the best faces seed clusters first.
    let mut faces = faces_for_clustering(conn, model_tag)?;
    let rejected: HashSet<(i64, i64)> = rejection_pairs(conn)?.into_iter().collect();
    let n = faces.len();
    let mut stats = ClusterStats::default();

    // Vectors are read IN PLACE (no n×dim matrix copy) — the neighbor scan stays EXACT pairwise, so
    // the validated 0.45 threshold needs no recalibration, and an incremental pass over a 100k-image
    // library doesn't allocate hundreds of MB. The outer loop only visits dirty (unassigned,
    // non-sticky) faces → O(dirty × n); a one-time bulk cluster is O(n²), dwarfed by detection at
    // scale — for >~200k faces, swap this exact scan for an ANN index (e.g. instant-distance HNSW).
    //
    // A face whose embedding length differs from the model's dim (corrupt / mixed model) is EXCLUDED
    // rather than zero-padded: padding would shrink cosine distance and cause false merges.
    let dim = faces.first().map(|f| f.vector.len()).unwrap_or(0);
    let valid: Vec<bool> = faces
        .iter()
        .map(|f| dim > 0 && f.vector.len() == dim)
        .collect();

    let tx = conn.transaction()?;
    let mut since_poll = 0usize;
    for i in 0..n {
        if !valid[i]
            || faces[i].person_id.is_some()
            || faces[i].status == "rejected"
            || faces[i].status == "ignored"
        {
            continue;
        }
        // Cooperative cancel: commit the (independent, idempotent) assignments done so far.
        since_poll += 1;
        if since_poll >= CANCEL_CHECK_EVERY {
            since_poll = 0;
            if cancel.load(Ordering::SeqCst) {
                break;
            }
        }
        // Tally neighbors within threshold (exact cosine distance = 1 − dot).
        let mut person_count: HashMap<i64, usize> = HashMap::new();
        let mut person_best: HashMap<i64, f32> = HashMap::new();
        let mut unassigned_neighbors: Vec<usize> = Vec::new();
        for j in 0..n {
            if j == i || !valid[j] || faces[j].status == "rejected" || faces[j].status == "ignored"
            {
                continue;
            }
            let d = cosine_dist(&faces[i].vector, &faces[j].vector);
            if d > p.max_distance {
                continue;
            }
            match faces[j].person_id {
                Some(pid) => {
                    if rejected.contains(&(faces[i].id, pid)) {
                        continue;
                    }
                    *person_count.entry(pid).or_default() += 1;
                    let e = person_best.entry(pid).or_insert(f32::MAX);
                    *e = e.min(d);
                }
                None => unassigned_neighbors.push(j),
            }
        }
        // Prefer joining an existing person (nearest among those clearing join_min).
        let best_person = person_count
            .iter()
            .filter(|(_, &c)| c >= p.join_min)
            .min_by(|a, b| {
                person_best[a.0]
                    .partial_cmp(&person_best[b.0])
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(&pid, _)| pid);
        if let Some(pid) = best_person {
            assign(&tx, faces[i].id, pid)?;
            faces[i].person_id = Some(pid);
            stats.assigned += 1;
            continue;
        }
        // Otherwise seed a new cluster from still-unassigned mutual neighbors.
        let fresh: Vec<usize> = unassigned_neighbors
            .into_iter()
            .filter(|&j| faces[j].person_id.is_none())
            .collect();
        if fresh.len() + 1 >= p.new_min {
            let pid = create_person(&tx, now)?;
            stats.new_people += 1;
            assign(&tx, faces[i].id, pid)?;
            faces[i].person_id = Some(pid);
            stats.assigned += 1;
            for &j in &fresh {
                if faces[j].person_id.is_none() {
                    assign(&tx, faces[j].id, pid)?;
                    faces[j].person_id = Some(pid);
                    stats.assigned += 1;
                }
            }
        } else {
            tx.execute(
                "UPDATE face SET deferred = 1 WHERE id = ?1",
                params![faces[i].id],
            )?;
            stats.deferred += 1;
        }
    }
    prune_empty_unnamed(&tx)?;
    let _ = now; // reserved for future per-assignment timestamps
    tx.commit()?;
    Ok(stats)
}

fn assign(conn: &Connection, face_id: i64, person_id: i64) -> Result<(), LibError> {
    conn.execute(
        "UPDATE face SET person_id = ?2, deferred = 0 WHERE id = ?1",
        params![face_id, person_id],
    )?;
    Ok(())
}

fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::face::{faces_for_clustering, list_people, reconcile_faces, FaceInput};
    use core_db::Db;

    const TAG: &str = "test_v1";

    fn face_with(emb: Vec<f32>) -> FaceInput {
        let mut e = emb;
        let n = e.iter().map(|x| x * x).sum::<f32>().sqrt();
        for x in &mut e {
            *x /= n;
        }
        FaceInput {
            bbox: [0.1, 0.1, 0.3, 0.3],
            kps: [0.0; 10],
            det_score: 0.9,
            quality: 1000.0,
            embedding: e,
        }
    }

    /// Two tight groups of 3 + a lone face → two people, the singleton deferred (unassigned).
    #[test]
    fn clusters_two_groups_defers_singleton() {
        let mut db = Db::open_in_memory().unwrap();
        // Seed 7 images (folder_id NULL) so each face's asset_id FK is satisfied.
        for id in 1..=7 {
            db.conn
                .execute(
                    "INSERT INTO images(id, content_hash, file_size, path, original_filename, status, imported_at)
                     VALUES (?1, X'00', 1, ?2, 'f', 'present', 0)",
                    params![id, format!("/img{id}")],
                )
                .unwrap();
        }
        // Group A near [1,0,0]; group B near [0,1,0]; singleton near [0,0,1].
        let groups: Vec<(i64, [f32; 3])> = vec![
            (1, [1.0, 0.02, 0.0]),
            (2, [1.0, 0.0, 0.03]),
            (3, [0.99, 0.01, 0.0]),
            (4, [0.0, 1.0, 0.02]),
            (5, [0.01, 1.0, 0.0]),
            (6, [0.0, 0.99, 0.02]),
            (7, [0.0, 0.0, 1.0]),
        ];
        {
            let tx = db.conn.transaction().unwrap();
            for (img, e) in &groups {
                reconcile_faces(&tx, *img, "mv", TAG, 0, &[face_with(e.to_vec())]).unwrap();
            }
            tx.commit().unwrap();
        }
        let stats = cluster_assign(
            &mut db.conn,
            TAG,
            0,
            ClusterParams::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(stats.new_people, 2, "two clusters");
        assert_eq!(stats.assigned, 6, "6 of 7 faces assigned");
        assert_eq!(stats.deferred, 1, "singleton deferred");
        let people = list_people(&db.conn, false).unwrap();
        assert_eq!(people.len(), 2);
        assert!(people.iter().all(|p| p.face_count == 3));
        // The singleton stayed unassigned.
        let unassigned = faces_for_clustering(&db.conn, TAG)
            .unwrap()
            .into_iter()
            .filter(|f| f.person_id.is_none())
            .count();
        assert_eq!(unassigned, 1);
    }

    /// A face whose embedding dim differs from the model's is EXCLUDED (not zero-padded into a false
    /// merge): it stays unassigned while the consistent-dim faces still cluster.
    #[test]
    fn mismatched_dim_face_excluded() {
        let mut db = Db::open_in_memory().unwrap();
        for id in 1..=4 {
            db.conn
                .execute(
                    "INSERT INTO images(id, content_hash, file_size, path, original_filename, status, imported_at)
                     VALUES (?1, X'00', 1, ?2, 'f', 'present', 0)",
                    params![id, format!("/img{id}")],
                )
                .unwrap();
        }
        {
            let tx = db.conn.transaction().unwrap();
            // Three consistent 3-dim faces (one cluster) + one corrupt 2-dim face near them.
            reconcile_faces(&tx, 1, "mv", TAG, 0, &[face_with(vec![1.0, 0.02, 0.0])]).unwrap();
            reconcile_faces(&tx, 2, "mv", TAG, 0, &[face_with(vec![1.0, 0.0, 0.03])]).unwrap();
            reconcile_faces(&tx, 3, "mv", TAG, 0, &[face_with(vec![0.99, 0.01, 0.0])]).unwrap();
            reconcile_faces(&tx, 4, "mv", TAG, 0, &[face_with(vec![1.0, 0.0])]).unwrap();
            tx.commit().unwrap();
        }
        cluster_assign(
            &mut db.conn,
            TAG,
            0,
            ClusterParams::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let faces = faces_for_clustering(&db.conn, TAG).unwrap();
        let corrupt = faces.iter().find(|f| f.vector.len() == 2).unwrap();
        assert!(
            corrupt.person_id.is_none(),
            "mismatched-dim face must not be clustered"
        );
        assert_eq!(
            list_people(&db.conn, false).unwrap().len(),
            1,
            "valid faces still cluster"
        );
    }
}
