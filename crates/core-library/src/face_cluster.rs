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

/// Faces at `model_tag` that are detected and durable but belong to no person yet.
///
/// Non-zero for two legitimate reasons: a face too isolated to clear `new_min` (deliberately
/// deferred), or a clustering pass that was interrupted. Either way the face exists and must not be
/// invisible — the People sidebar reports this as "N ungrouped faces".
pub fn ungrouped_face_count(conn: &Connection, model_tag: &str) -> Result<i64, LibError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM face f JOIN face_embedding e ON e.face_id = f.id
          WHERE e.model_tag = ?1 AND f.person_id IS NULL
            AND f.status NOT IN ('rejected','ignored')",
        params![model_tag],
        |r| r.get(0),
    )?)
}

/// True if any face at `model_tag` still needs clustering (unassigned + non-sticky). Lets the caller
/// skip the whole pass when nothing changed — clustering is otherwise re-run on every scan.
pub fn has_dirty_faces(conn: &Connection, model_tag: &str) -> Result<bool, LibError> {
    Ok(ungrouped_face_count(conn, model_tag)? > 0)
}

/// Everything the clustering pass needs, read once so the expensive scan can run with no DB lock
/// held. Faces are quality-sorted (best seed clusters first) exactly as the query returns them.
pub struct ClusterSnapshot {
    pub faces: Vec<crate::face::ClusterFace>,
    pub rejected: HashSet<(i64, i64)>,
}

/// Which person an assignment targets. `New(k)` refers to the k-th person the plan asks to create —
/// it cannot be a real id yet, because the pure pass has no DB to create one in. A face may join a
/// cluster seeded earlier in the same pass, so this indirection is load-bearing, not cosmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonRef {
    Existing(i64),
    New(usize),
}

/// The outcome of a pure clustering pass: create `new_people` persons, apply `assignments`, mark
/// `deferred`. Holding this as data (rather than writing as we go) is what lets the expensive
/// neighbour scan run outside the DB lock.
#[derive(Debug, Default)]
pub struct ClusterPlan {
    pub new_people: usize,
    pub assignments: Vec<(i64, PersonRef)>,
    pub deferred: Vec<i64>,
}

impl ClusterPlan {
    pub fn stats(&self) -> ClusterStats {
        ClusterStats {
            assigned: self.assignments.len(),
            new_people: self.new_people,
            deferred: self.deferred.len(),
        }
    }
}

/// Step 1 of 3 — read the clustering input. **Hold the DB lock only for this.**
pub fn cluster_snapshot(conn: &Connection, model_tag: &str) -> Result<ClusterSnapshot, LibError> {
    Ok(ClusterSnapshot {
        // Quality-sorted so the best faces seed clusters first.
        faces: faces_for_clustering(conn, model_tag)?,
        rejected: rejection_pairs(conn)?.into_iter().collect(),
    })
}

/// Step 2 of 3 — the actual clustering. **Pure: no DB access, so run it with NO lock held.**
///
/// This is `O(dirty × n)` float work; running it under the single shared connection is what used to
/// make a long clustering phase block every other DB-backed UI action. `cancel` is polled every
/// [`CANCEL_CHECK_EVERY`] candidates and `on_progress(done, total)` fires at the same cadence, so a
/// long pass is both visible and interruptible. Cancelling returns the plan built so far —
/// assignments are independent and idempotent, and unprocessed faces simply stay unassigned.
///
/// Vectors are compared IN PLACE (no n×dim matrix copy) — the neighbour scan stays EXACT pairwise,
/// so the validated 0.45 threshold needs no recalibration, and an incremental pass over a 100k-image
/// library doesn't allocate hundreds of MB. For >~200k faces, swap this exact scan for an ANN index
/// (e.g. instant-distance HNSW).
///
/// A face whose embedding length differs from the model's dim (corrupt / mixed model) is EXCLUDED
/// rather than zero-padded: padding would shrink cosine distance and cause false merges.
pub fn plan_clusters(
    snap: &ClusterSnapshot,
    p: ClusterParams,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(usize, usize),
) -> ClusterPlan {
    let faces = &snap.faces;
    let n = faces.len();
    let mut plan = ClusterPlan::default();

    let dim = faces.first().map(|f| f.vector.len()).unwrap_or(0);
    let valid: Vec<bool> = faces
        .iter()
        .map(|f| dim > 0 && f.vector.len() == dim)
        .collect();

    // Working copy of each face's owner, updated as the pass places faces — a face seeded into a new
    // cluster must look "assigned" to later candidates, exactly as before.
    let mut owner: Vec<Option<PersonRef>> = faces
        .iter()
        .map(|f| f.person_id.map(PersonRef::Existing))
        .collect();
    let skip =
        |i: usize| !valid[i] || faces[i].status == "rejected" || faces[i].status == "ignored";

    let dirty_total = (0..n).filter(|&i| !skip(i) && owner[i].is_none()).count();
    let mut visited = 0usize;
    let mut since_poll = 0usize;
    for i in 0..n {
        if skip(i) || owner[i].is_some() {
            continue;
        }
        visited += 1;
        since_poll += 1;
        if since_poll >= CANCEL_CHECK_EVERY {
            since_poll = 0;
            on_progress(visited, dirty_total);
            if cancel.load(Ordering::SeqCst) {
                break;
            }
        }
        // Tally neighbours within threshold (exact cosine distance = 1 − dot).
        let mut person_count: HashMap<i64, usize> = HashMap::new();
        let mut person_best: HashMap<i64, f32> = HashMap::new();
        let mut unassigned_neighbors: Vec<usize> = Vec::new();
        for j in 0..n {
            if j == i || skip(j) {
                continue;
            }
            let d = cosine_dist(&faces[i].vector, &faces[j].vector);
            if d > p.max_distance {
                continue;
            }
            match owner[j] {
                // Only an already-existing person can carry a rejection pair; a person created by
                // this very pass cannot have been rejected against yet.
                Some(PersonRef::Existing(pid)) => {
                    if snap.rejected.contains(&(faces[i].id, pid)) {
                        continue;
                    }
                    *person_count.entry(pid).or_default() += 1;
                    let e = person_best.entry(pid).or_insert(f32::MAX);
                    *e = e.min(d);
                }
                Some(PersonRef::New(_)) => {}
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
            let r = PersonRef::Existing(pid);
            plan.assignments.push((faces[i].id, r));
            owner[i] = Some(r);
            continue;
        }
        // Otherwise seed a new cluster from still-unassigned mutual neighbours.
        let fresh: Vec<usize> = unassigned_neighbors
            .into_iter()
            .filter(|&j| owner[j].is_none())
            .collect();
        if fresh.len() + 1 >= p.new_min {
            let r = PersonRef::New(plan.new_people);
            plan.new_people += 1;
            plan.assignments.push((faces[i].id, r));
            owner[i] = Some(r);
            for &j in &fresh {
                if owner[j].is_none() {
                    plan.assignments.push((faces[j].id, r));
                    owner[j] = Some(r);
                }
            }
        } else {
            plan.deferred.push(faces[i].id);
        }
    }
    on_progress(visited, dirty_total);
    plan
}

/// Step 3 of 3 — write the plan. **Hold the DB lock only for this**: it is a flat sequence of
/// keyed UPDATEs, not the quadratic scan.
pub fn apply_cluster_plan(
    conn: &mut Connection,
    now: i64,
    plan: &ClusterPlan,
) -> Result<ClusterStats, LibError> {
    let tx = conn.transaction()?;
    let mut created: Vec<i64> = Vec::with_capacity(plan.new_people);
    for _ in 0..plan.new_people {
        created.push(create_person(&tx, now)?);
    }
    for (face_id, who) in &plan.assignments {
        let pid = match who {
            PersonRef::Existing(id) => *id,
            PersonRef::New(k) => created[*k],
        };
        assign(&tx, *face_id, pid)?;
    }
    for face_id in &plan.deferred {
        tx.execute(
            "UPDATE face SET deferred = 1 WHERE id = ?1",
            params![face_id],
        )?;
    }
    prune_empty_unnamed(&tx)?;
    tx.commit()?;
    Ok(plan.stats())
}

/// All three steps against one connection. **Holds the lock for the whole pass**, so callers that
/// share the connection with the UI (i.e. the app) must use the three steps separately; this
/// convenience wrapper is for tests and offline tools.
pub fn cluster_assign(
    conn: &mut Connection,
    model_tag: &str,
    now: i64,
    p: ClusterParams,
    cancel: &AtomicBool,
) -> Result<ClusterStats, LibError> {
    let snap = cluster_snapshot(conn, model_tag)?;
    let plan = plan_clusters(&snap, p, cancel, &mut |_, _| {});
    apply_cluster_plan(conn, now, &plan)
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

    /// Seeds `n` images each carrying one face at `embeddings[i]`.
    fn seed(db: &mut Db, embeddings: &[Vec<f32>]) {
        for id in 1..=embeddings.len() as i64 {
            db.conn
                .execute(
                    "INSERT INTO images(id, content_hash, file_size, path, original_filename, status, imported_at)
                     VALUES (?1, X'00', 1, ?2, 'f', 'present', 0)",
                    params![id, format!("/img{id}")],
                )
                .unwrap();
        }
        let tx = db.conn.transaction().unwrap();
        for (i, e) in embeddings.iter().enumerate() {
            reconcile_faces(&tx, i as i64 + 1, "mv", TAG, 0, &[face_with(e.clone())]).unwrap();
        }
        tx.commit().unwrap();
    }

    /// The plan is pure: producing it must not touch the DB, so a caller can hold no lock while the
    /// quadratic scan runs. Nothing is written until `apply_cluster_plan`.
    #[test]
    fn planning_writes_nothing_until_applied() {
        let mut db = Db::open_in_memory().unwrap();
        seed(
            &mut db,
            &[
                vec![1.0, 0.02, 0.0],
                vec![1.0, 0.0, 0.03],
                vec![0.99, 0.01, 0.0],
            ],
        );
        let snap = cluster_snapshot(&db.conn, TAG).unwrap();
        let plan = plan_clusters(
            &snap,
            ClusterParams::default(),
            &AtomicBool::new(false),
            &mut |_, _| {},
        );
        assert_eq!(plan.new_people, 1);
        assert_eq!(plan.assignments.len(), 3);
        assert_eq!(
            list_people(&db.conn, false).unwrap().len(),
            0,
            "planning must not create people — it has no DB access at all"
        );

        apply_cluster_plan(&mut db.conn, 0, &plan).unwrap();
        assert_eq!(list_people(&db.conn, false).unwrap().len(), 1);
        assert_eq!(ungrouped_face_count(&db.conn, TAG).unwrap(), 0);
    }

    /// A face may join a cluster seeded EARLIER in the same pass, before that person exists in the
    /// DB. The `PersonRef::New` indirection is what keeps that correct.
    #[test]
    fn a_face_can_join_a_cluster_seeded_in_the_same_pass() {
        let mut db = Db::open_in_memory().unwrap();
        // Four near-identical faces: the first three seed a cluster, the fourth must join it.
        seed(
            &mut db,
            &[
                vec![1.0, 0.02, 0.0],
                vec![1.0, 0.0, 0.03],
                vec![0.99, 0.01, 0.0],
                vec![1.0, 0.015, 0.01],
            ],
        );
        let snap = cluster_snapshot(&db.conn, TAG).unwrap();
        let plan = plan_clusters(
            &snap,
            ClusterParams::default(),
            &AtomicBool::new(false),
            &mut |_, _| {},
        );
        assert_eq!(plan.new_people, 1, "exactly one person for all four faces");
        assert!(
            plan.assignments
                .iter()
                .all(|(_, who)| *who == PersonRef::New(0)),
            "every face lands in the same new cluster"
        );
        apply_cluster_plan(&mut db.conn, 0, &plan).unwrap();
        let people = list_people(&db.conn, false).unwrap();
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].face_count, 4);
    }

    /// Interrupting the pass yields a valid PARTIAL plan and loses nothing.
    ///
    /// Cancellation is polled every [`CANCEL_CHECK_EVERY`] candidates, so this needs enough
    /// mutually-distant faces to cross that boundary — a handful of faces would finish before the
    /// first poll and the cancel would (correctly) never be observed.
    #[test]
    fn cancelling_the_pass_yields_a_partial_plan_and_loses_no_faces() {
        let n = CANCEL_CHECK_EVERY + 20;
        // One-hot vectors: every pair is at cosine distance 1.0, far beyond max_distance, so no face
        // ever clusters and each one is visited as its own candidate.
        let embeddings: Vec<Vec<f32>> = (0..n)
            .map(|k| {
                let mut v = vec![0.0; n];
                v[k] = 1.0;
                v
            })
            .collect();
        let mut db = Db::open_in_memory().unwrap();
        seed(&mut db, &embeddings);
        let snap = cluster_snapshot(&db.conn, TAG).unwrap();

        let full = plan_clusters(
            &snap,
            ClusterParams::default(),
            &AtomicBool::new(false),
            &mut |_, _| {},
        );
        let partial = plan_clusters(
            &snap,
            ClusterParams::default(),
            &AtomicBool::new(true),
            &mut |_, _| {},
        );
        assert_eq!(
            full.deferred.len(),
            n,
            "an uninterrupted pass visits every face"
        );
        assert!(
            partial.deferred.len() < full.deferred.len(),
            "cancelling must stop the pass early ({} vs {})",
            partial.deferred.len(),
            full.deferred.len()
        );

        apply_cluster_plan(&mut db.conn, 0, &partial).unwrap();
        assert_eq!(
            faces_for_clustering(&db.conn, TAG).unwrap().len(),
            n,
            "every face row and embedding survives an interrupted pass"
        );
        assert_eq!(
            ungrouped_face_count(&db.conn, TAG).unwrap(),
            n as i64,
            "and all are reported as ungrouped rather than silently vanishing"
        );
    }

    /// A deliberately-deferred singleton is ungrouped too — the count covers both reasons.
    #[test]
    fn deferred_singleton_counts_as_ungrouped() {
        let mut db = Db::open_in_memory().unwrap();
        seed(
            &mut db,
            &[
                vec![1.0, 0.02, 0.0],
                vec![1.0, 0.0, 0.03],
                vec![0.99, 0.01, 0.0],
                vec![0.0, 0.0, 1.0], // isolated
            ],
        );
        cluster_assign(
            &mut db.conn,
            TAG,
            0,
            ClusterParams::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(ungrouped_face_count(&db.conn, TAG).unwrap(), 1);
    }

    /// Progress must be reported against the DIRTY count, not the whole table, or the pill would
    /// stall at a fraction of a library that is mostly already clustered.
    #[test]
    fn progress_reports_against_the_dirty_total() {
        let mut db = Db::open_in_memory().unwrap();
        let mut embeddings: Vec<Vec<f32>> = (0..CANCEL_CHECK_EVERY + 20)
            .map(|k| vec![1.0, k as f32 * 0.0001, 0.0])
            .collect();
        embeddings.push(vec![0.0, 1.0, 0.0]);
        seed(&mut db, &embeddings);
        let snap = cluster_snapshot(&db.conn, TAG).unwrap();
        let mut seen: Vec<(usize, usize)> = Vec::new();
        let plan = plan_clusters(
            &snap,
            ClusterParams::default(),
            &AtomicBool::new(false),
            &mut |done, total| seen.push((done, total)),
        );
        assert!(!seen.is_empty(), "a long pass must report progress");
        let total = seen[0].1;
        assert_eq!(
            total,
            embeddings.len(),
            "total is the number of faces needing placement"
        );
        assert!(
            seen.iter().all(|(done, _)| *done <= total),
            "progress never exceeds its total"
        );
        apply_cluster_plan(&mut db.conn, 0, &plan).unwrap();
    }
}
