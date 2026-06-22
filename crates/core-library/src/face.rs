//! Persistence + read/label queries for faces & people.
//!
//! Mirrors the `analysis.rs` discipline (bound params, free of any ml/ort dependency). Faces are
//! written by the `src-tauri` face pass via [`reconcile_faces`]; clustering ([`crate::face_cluster`])
//! and the People IPC read/mutate through here. Embeddings are stored as little-endian f32 BLOBs.

use core_db::rusqlite::{named_params, params, Connection, OptionalExtension};
use core_raw::hex;
use serde::Serialize;

use crate::error::LibError;

/// Face statuses that count as "belongs to this person" for counts, filters, and thumbnails.
const ACTIVE_STATUS: &str = "('confirmed','unconfirmed')";

/// One detected+embedded face to persist (local mirror of `core_analyze::FaceRecord`).
pub struct FaceInput {
    /// Normalized `[x1,y1,x2,y2]` in `[0,1]`.
    pub bbox: [f32; 4],
    /// 5 landmark pairs (10 f32), source pixels.
    pub kps: [f32; 10],
    pub det_score: f32,
    pub quality: f32,
    /// L2-normalized embedding.
    pub embedding: Vec<f32>,
}

fn f32s_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn blob_to_f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Min IoU for a fresh detection to be considered the SAME physical face as a stored row.
const FACE_MATCH_IOU: f64 = 0.5;

/// Intersection-over-union of two normalized `[x1,y1,x2,y2]` boxes.
fn bbox_iou(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let iw = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let ih = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let inter = iw * ih;
    let area_a = ((a[2] - a[0]) * (a[3] - a[1])).max(0.0);
    let area_b = ((b[2] - b[0]) * (b[3] - b[1])).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

struct ExistingFace {
    id: i64,
    bbox: [f64; 4],
    status: String,
    source: String,
    person_id: Option<i64>,
}

/// Insert one fresh detected face (+ its embedding) as a new unconfirmed `ml` row.
fn insert_one_face(
    conn: &Connection,
    image_id: i64,
    model_version: &str,
    model_tag: &str,
    now: i64,
    f: &FaceInput,
) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO face
           (asset_id, person_id, bbox_x1, bbox_y1, bbox_x2, bbox_y2, kps, quality_score,
            det_score, source, status, deferred, model_version, created_at)
         VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'ml', 'unconfirmed', 1, ?9, ?10)",
        params![
            image_id,
            f.bbox[0] as f64,
            f.bbox[1] as f64,
            f.bbox[2] as f64,
            f.bbox[3] as f64,
            f32s_to_blob(&f.kps),
            f.quality as f64,
            f.det_score as f64,
            model_version,
            now,
        ],
    )?;
    let face_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO face_embedding (face_id, dim, vector, model_tag) VALUES (?1, ?2, ?3, ?4)",
        params![
            face_id,
            f.embedding.len() as i64,
            f32s_to_blob(&f.embedding),
            model_tag,
        ],
    )?;
    Ok(())
}

/// Reconcile this image's detected faces with what's already stored — **re-run safe without losing
/// user decisions**. New detections are matched to existing faces by bounding-box IoU
/// (≥ [`FACE_MATCH_IOU`], greedily, 1:1). A match KEEPS the existing face id, `person_id`, `status`
/// (confirmed/rejected), and any cover reference while refreshing geometry + replacing the embedding
/// (so a `model_tag` change re-embeds in place). Unmatched detections are inserted as fresh
/// unconfirmed faces. Existing faces with no match are dropped ONLY if they were unconfirmed
/// auto-detections — `confirmed`/`rejected`/`manual` faces are preserved, so a transient detector
/// miss never wipes a named person, its embeddings, or its rejection history. MUST run inside a
/// transaction. Returns the number of faces detected this run.
pub fn reconcile_faces(
    conn: &Connection,
    image_id: i64,
    model_version: &str,
    model_tag: &str,
    now: i64,
    faces: &[FaceInput],
) -> Result<usize, LibError> {
    let existing: Vec<ExistingFace> = {
        let mut stmt = conn.prepare(
            "SELECT id, bbox_x1, bbox_y1, bbox_x2, bbox_y2, status, source, person_id
               FROM face WHERE asset_id = ?1",
        )?;
        let rows = stmt.query_map(params![image_id], |r| {
            Ok(ExistingFace {
                id: r.get(0)?,
                bbox: [r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?],
                status: r.get(5)?,
                source: r.get(6)?,
                person_id: r.get(7)?,
            })
        })?;
        rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?
    };

    // Greedy IoU matching (highest overlap first, 1:1) of new detection → existing face.
    let mut pairs: Vec<(f64, usize, usize)> = Vec::new();
    for (ni, f) in faces.iter().enumerate() {
        let nb = [
            f.bbox[0] as f64,
            f.bbox[1] as f64,
            f.bbox[2] as f64,
            f.bbox[3] as f64,
        ];
        for (ei, e) in existing.iter().enumerate() {
            let iou = bbox_iou(&nb, &e.bbox);
            if iou >= FACE_MATCH_IOU {
                pairs.push((iou, ni, ei));
            }
        }
    }
    pairs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut new_to_existing: Vec<Option<i64>> = vec![None; faces.len()];
    let mut new_matched = vec![false; faces.len()];
    let mut existing_matched = vec![false; existing.len()];
    for (_, ni, ei) in pairs {
        if !new_matched[ni] && !existing_matched[ei] {
            new_matched[ni] = true;
            existing_matched[ei] = true;
            new_to_existing[ni] = Some(existing[ei].id);
        }
    }

    for (ni, f) in faces.iter().enumerate() {
        match new_to_existing[ni] {
            // Matched: refresh geometry + scores + version, keep id/person/status/cover; re-embed.
            Some(fid) => {
                conn.execute(
                    "UPDATE face SET bbox_x1=?2, bbox_y1=?3, bbox_x2=?4, bbox_y2=?5, kps=?6,
                       quality_score=?7, det_score=?8, model_version=?9 WHERE id=?1",
                    params![
                        fid,
                        f.bbox[0] as f64,
                        f.bbox[1] as f64,
                        f.bbox[2] as f64,
                        f.bbox[3] as f64,
                        f32s_to_blob(&f.kps),
                        f.quality as f64,
                        f.det_score as f64,
                        model_version,
                    ],
                )?;
                conn.execute(
                    "INSERT OR REPLACE INTO face_embedding (face_id, dim, vector, model_tag)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        fid,
                        f.embedding.len() as i64,
                        f32s_to_blob(&f.embedding),
                        model_tag,
                    ],
                )?;
            }
            None => insert_one_face(conn, image_id, model_version, model_tag, now, f)?,
        }
    }

    // Drop stale auto-detections that no longer match a detection — but ONLY truly unattached ones.
    // A face clustering assigned to a person keeps `person_id` while `status` stays 'unconfirmed';
    // deleting it would orphan the person and lose the embedding, so `person_id.is_some()` is
    // preserved alongside confirmed/rejected/manual faces.
    for (ei, e) in existing.iter().enumerate() {
        if existing_matched[ei] {
            continue;
        }
        let keep = e.status == "confirmed"
            || e.status == "rejected"
            || e.source == "manual"
            || e.person_id.is_some();
        if !keep {
            conn.execute("DELETE FROM face WHERE id = ?1", params![e.id])?;
        }
    }
    Ok(faces.len())
}

// ---------- clustering input ----------

/// A face + its embedding for the clustering pass.
pub struct ClusterFace {
    pub id: i64,
    pub person_id: Option<i64>,
    pub status: String,
    pub quality: f64,
    pub vector: Vec<f32>,
}

/// All faces with an embedding at `model_tag`, joined to their vector (clustering input).
pub fn faces_for_clustering(
    conn: &Connection,
    model_tag: &str,
) -> Result<Vec<ClusterFace>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.person_id, f.status, f.quality_score, e.vector
           FROM face f JOIN face_embedding e ON e.face_id = f.id
          WHERE e.model_tag = ?1
          ORDER BY f.quality_score DESC, f.id",
    )?;
    let rows = stmt.query_map(params![model_tag], |r| {
        let blob: Vec<u8> = r.get(4)?;
        Ok(ClusterFace {
            id: r.get(0)?,
            person_id: r.get(1)?,
            status: r.get(2)?,
            quality: r.get(3)?,
            vector: blob_to_f32s(&blob),
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// All `(face_id, person_id)` rejection pairs ("not this person").
pub fn rejection_pairs(conn: &Connection) -> Result<Vec<(i64, i64)>, LibError> {
    let mut stmt = conn.prepare("SELECT face_id, person_id FROM face_rejection")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

// ---------- read side (IPC) ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonRow {
    pub id: i64,
    pub name: Option<String>,
    pub hidden: bool,
    pub face_count: i64,
    /// Cover face + its image hash and box, so the frontend can CSS-crop a thumbnail from `thumb://`.
    pub cover_face_id: Option<i64>,
    pub cover_image_hash: Option<String>,
    pub cover_bbox: Option<[f64; 4]>,
}

/// People for the sidebar: named first then unnamed "Suggested", each with its cover crop. Empty
/// clusters and (unless `include_hidden`) hidden people are omitted.
pub fn list_people(conn: &Connection, include_hidden: bool) -> Result<Vec<PersonRow>, LibError> {
    let sql = format!(
        "SELECT p.id, p.name, p.hidden,
            (SELECT COUNT(*) FROM face f WHERE f.person_id = p.id AND f.status IN {ACTIVE_STATUS}) AS cnt,
            cover.id, img.content_hash, cover.bbox_x1, cover.bbox_y1, cover.bbox_x2, cover.bbox_y2
         FROM person p
         LEFT JOIN face cover ON cover.id = COALESCE(
            p.thumbnail_face_id,
            (SELECT f2.id FROM face f2 WHERE f2.person_id = p.id AND f2.status IN {ACTIVE_STATUS}
              ORDER BY f2.quality_score DESC LIMIT 1))
         LEFT JOIN images img ON img.id = cover.asset_id
         WHERE (:include_hidden OR p.hidden = 0)
         ORDER BY (p.name IS NULL), p.name COLLATE NOCASE, p.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(named_params! { ":include_hidden": include_hidden }, |r| {
        let cnt: i64 = r.get(3)?;
        let cover_face_id: Option<i64> = r.get(4)?;
        let hash_bytes: Option<Vec<u8>> = r.get(5)?;
        let cover_image_hash = hash_bytes.and_then(|b| {
            (b.len() == 32).then(|| {
                let mut a = [0u8; 32];
                a.copy_from_slice(&b);
                hex(&a)
            })
        });
        let bbox = match (r.get::<_, Option<f64>>(6)?, cover_face_id) {
            (Some(x1), Some(_)) => Some([x1, r.get(7)?, r.get(8)?, r.get(9)?]),
            _ => None,
        };
        Ok(PersonRow {
            id: r.get(0)?,
            name: r.get(1)?,
            hidden: r.get::<_, i64>(2)? != 0,
            face_count: cnt,
            cover_face_id,
            cover_image_hash,
            cover_bbox: bbox,
        })
    })?;
    Ok(rows
        .collect::<core_db::rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|p| p.face_count > 0)
        .collect())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonFaceRow {
    pub id: i64,
    pub image_id: i64,
    pub image_hash: String,
    pub bbox: [f64; 4],
    pub status: String,
    pub det_score: f64,
    pub quality: f64,
}

/// Faces of a person, optionally restricted to a status (e.g. `"unconfirmed"` for the Review flow).
pub fn person_faces(
    conn: &Connection,
    person_id: i64,
    status: Option<&str>,
) -> Result<Vec<PersonFaceRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.asset_id, i.content_hash, f.bbox_x1, f.bbox_y1, f.bbox_x2, f.bbox_y2,
                f.status, f.det_score, f.quality_score
           FROM face f JOIN images i ON i.id = f.asset_id
          WHERE f.person_id = ?1 AND (?2 IS NULL OR f.status = ?2)
          ORDER BY f.quality_score DESC, f.id",
    )?;
    let rows = stmt.query_map(params![person_id, status], map_person_face)?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

fn map_person_face(r: &core_db::rusqlite::Row<'_>) -> core_db::rusqlite::Result<PersonFaceRow> {
    let hash_bytes: Vec<u8> = r.get(2)?;
    let image_hash = if hash_bytes.len() == 32 {
        let mut a = [0u8; 32];
        a.copy_from_slice(&hash_bytes);
        hex(&a)
    } else {
        String::new()
    };
    Ok(PersonFaceRow {
        id: r.get(0)?,
        image_id: r.get(1)?,
        image_hash,
        bbox: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
        status: r.get(7)?,
        det_score: r.get(8)?,
        quality: r.get(9)?,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageFaceRow {
    pub id: i64,
    pub person_id: Option<i64>,
    pub person_name: Option<String>,
    pub bbox: [f64; 4],
    pub status: String,
}

/// Faces detected in one image, joined to their (optional) person name — the RightInfo "People" chips.
pub fn image_faces(conn: &Connection, image_id: i64) -> Result<Vec<ImageFaceRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.person_id, p.name, f.bbox_x1, f.bbox_y1, f.bbox_x2, f.bbox_y2, f.status
           FROM face f LEFT JOIN person p ON p.id = f.person_id
          WHERE f.asset_id = ?1 AND f.status != 'ignored'
          ORDER BY f.bbox_x1",
    )?;
    let rows = stmt.query_map(params![image_id], |r| {
        Ok(ImageFaceRow {
            id: r.get(0)?,
            person_id: r.get(1)?,
            person_name: r.get(2)?,
            bbox: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
            status: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

// ---------- mutations (label ops) ----------

/// Create a new (unnamed) person, returning its id.
pub fn create_person(conn: &Connection, now: i64) -> Result<i64, LibError> {
    conn.execute(
        "INSERT INTO person (name, hidden, created_at, updated_at) VALUES (NULL, 0, ?1, ?1)",
        params![now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Set (or clear, with `None`) a person's name.
pub fn set_person_name(
    conn: &Connection,
    person_id: i64,
    name: Option<&str>,
    now: i64,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE person SET name = ?2, updated_at = ?3 WHERE id = ?1",
        params![person_id, name, now],
    )?;
    Ok(())
}

pub fn set_person_hidden(
    conn: &Connection,
    person_id: i64,
    hidden: bool,
    now: i64,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE person SET hidden = ?2, updated_at = ?3 WHERE id = ?1",
        params![person_id, hidden as i64, now],
    )?;
    Ok(())
}

/// Pick a person's cover (key) face. The face must belong to the person.
pub fn set_person_cover(
    conn: &Connection,
    person_id: i64,
    face_id: i64,
    now: i64,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE person SET thumbnail_face_id = ?2, updated_at = ?3
           WHERE id = ?1 AND EXISTS (SELECT 1 FROM face f WHERE f.id = ?2 AND f.person_id = ?1)",
        params![person_id, face_id, now],
    )?;
    Ok(())
}

/// Confirm a face (user said "yes, this is them").
pub fn confirm_face(conn: &Connection, face_id: i64) -> Result<(), LibError> {
    conn.execute(
        "UPDATE face SET status = 'confirmed', deferred = 0 WHERE id = ?1",
        params![face_id],
    )?;
    Ok(())
}

/// Reject a face from its current person ("not this person"): unlink it and record the rejection so
/// clustering never re-suggests the pair.
pub fn reject_face(conn: &Connection, face_id: i64) -> Result<(), LibError> {
    if let Some(person_id) = conn
        .query_row(
            "SELECT person_id FROM face WHERE id = ?1",
            params![face_id],
            |r| r.get::<_, Option<i64>>(0),
        )
        .optional()?
        .flatten()
    {
        conn.execute(
            "INSERT OR IGNORE INTO face_rejection (face_id, person_id) VALUES (?1, ?2)",
            params![face_id, person_id],
        )?;
    }
    conn.execute(
        "UPDATE face SET person_id = NULL, status = 'rejected', deferred = 0 WHERE id = ?1",
        params![face_id],
    )?;
    Ok(())
}

/// Merge `src` into `dst`: move all faces + rejections, inherit a cover if `dst` lacks one, delete
/// `src`. MUST run inside a transaction.
pub fn merge_people(conn: &Connection, dst: i64, src: i64) -> Result<(), LibError> {
    if dst == src {
        return Ok(());
    }
    conn.execute(
        "UPDATE face SET person_id = ?1 WHERE person_id = ?2",
        params![dst, src],
    )?;
    conn.execute(
        "UPDATE OR IGNORE face_rejection SET person_id = ?1 WHERE person_id = ?2",
        params![dst, src],
    )?;
    conn.execute(
        "UPDATE person SET thumbnail_face_id = (SELECT thumbnail_face_id FROM person WHERE id = ?2)
           WHERE id = ?1 AND thumbnail_face_id IS NULL",
        params![dst, src],
    )?;
    conn.execute("DELETE FROM person WHERE id = ?1", params![src])?;
    Ok(())
}

/// Reassign a face to a person (manual), confirming it. `None` unlinks (back to the suggestion pool).
pub fn assign_face_person(
    conn: &Connection,
    face_id: i64,
    person_id: Option<i64>,
) -> Result<(), LibError> {
    match person_id {
        Some(pid) => conn.execute(
            "UPDATE face SET person_id = ?2, status = 'confirmed', source = 'manual', deferred = 0
               WHERE id = ?1",
            params![face_id, pid],
        )?,
        None => conn.execute(
            "UPDATE face SET person_id = NULL, status = 'unconfirmed', deferred = 1 WHERE id = ?1",
            params![face_id],
        )?,
    };
    Ok(())
}

/// Delete every unnamed person that has no remaining active face (housekeeping after re-cluster/merge).
pub fn prune_empty_unnamed(conn: &Connection) -> Result<usize, LibError> {
    let n = conn.execute(
        &format!(
            "DELETE FROM person WHERE name IS NULL AND NOT EXISTS
               (SELECT 1 FROM face f WHERE f.person_id = person.id AND f.status IN {ACTIVE_STATUS})"
        ),
        [],
    )?;
    Ok(n)
}

/// Count of present images that have ≥1 detected face (People status denominator/summary).
pub fn faces_summary(conn: &Connection) -> Result<(i64, i64), LibError> {
    let faces: i64 = conn.query_row("SELECT COUNT(*) FROM face", [], |r| r.get(0))?;
    let people: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM person p WHERE EXISTS
               (SELECT 1 FROM face f WHERE f.person_id = p.id AND f.status IN {ACTIVE_STATUS})"
        ),
        [],
        |r| r.get(0),
    )?;
    Ok((faces, people))
}

/// Wipe ALL face/person data (the privacy "Delete all face data" action) and the face-pass markers in
/// `analysis_results`, so a later run re-processes from scratch. MUST run inside a transaction.
pub fn delete_all_face_data(conn: &Connection) -> Result<(), LibError> {
    conn.execute("DELETE FROM face", [])?; // cascades face_embedding + face_rejection
    conn.execute("DELETE FROM person", [])?;
    conn.execute(
        "DELETE FROM analysis_results WHERE analyzer_id = 'face_detection'",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::Db;

    fn seed_image(db: &Db, id: i64) {
        db.conn
            .execute(
                "INSERT INTO images(id, content_hash, file_size, path, original_filename, status, imported_at)
                 VALUES (?1, X'00', 1, ?2, 'f', 'present', 0)",
                params![id, format!("/img{id}")],
            )
            .unwrap();
    }

    fn face_at(bbox: [f32; 4]) -> FaceInput {
        FaceInput {
            bbox,
            kps: [0.0; 10],
            det_score: 0.9,
            quality: 1.0,
            embedding: vec![1.0, 0.0, 0.0],
        }
    }

    fn faces_of(db: &Db, asset: i64) -> Vec<(i64, Option<i64>, String)> {
        let mut stmt = db
            .conn
            .prepare("SELECT id, person_id, status FROM face WHERE asset_id = ?1 ORDER BY id")
            .unwrap();
        let rows = stmt
            .query_map(params![asset], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        rows.collect::<core_db::rusqlite::Result<Vec<_>>>().unwrap()
    }

    /// A moved-but-same face keeps its id/person/confirmed status; an unconfirmed face that vanishes
    /// from detection is dropped.
    #[test]
    fn reconcile_preserves_identity_and_drops_stale_unconfirmed() {
        let db = Db::open_in_memory().unwrap();
        seed_image(&db, 1);
        reconcile_faces(
            &db.conn,
            1,
            "mv",
            "tag",
            0,
            &[face_at([0.1, 0.1, 0.3, 0.3]), face_at([0.6, 0.6, 0.8, 0.8])],
        )
        .unwrap();
        let before = faces_of(&db, 1);
        assert_eq!(before.len(), 2);
        let a_id = before[0].0;
        let person = create_person(&db.conn, 0).unwrap();
        assign_face_person(&db.conn, a_id, Some(person)).unwrap();

        // Re-scan: A moved slightly (same physical face, high IoU), B gone.
        reconcile_faces(
            &db.conn,
            1,
            "mv2",
            "tag",
            1,
            &[face_at([0.11, 0.11, 0.31, 0.31])],
        )
        .unwrap();
        let after = faces_of(&db, 1);
        assert_eq!(after.len(), 1, "stale unconfirmed B dropped, A kept");
        assert_eq!(after[0].0, a_id, "A keeps its stable id");
        assert_eq!(after[0].1, Some(person), "A keeps its person");
        assert_eq!(after[0].2, "confirmed", "A stays confirmed");
    }

    /// A confirmed face the detector no longer finds (occluded/turned this run) must NOT be wiped.
    #[test]
    fn reconcile_keeps_confirmed_face_with_no_detection() {
        let db = Db::open_in_memory().unwrap();
        seed_image(&db, 1);
        reconcile_faces(
            &db.conn,
            1,
            "mv",
            "tag",
            0,
            &[face_at([0.1, 0.1, 0.3, 0.3])],
        )
        .unwrap();
        let id = faces_of(&db, 1)[0].0;
        let person = create_person(&db.conn, 0).unwrap();
        assign_face_person(&db.conn, id, Some(person)).unwrap();

        reconcile_faces(&db.conn, 1, "mv", "tag", 1, &[]).unwrap();
        let after = faces_of(&db, 1);
        assert_eq!(
            after.len(),
            1,
            "confirmed face survives a zero-detection re-scan"
        );
        assert_eq!(after[0].2, "confirmed");
    }

    /// A face clustering auto-assigned to a person (person_id set, status still 'unconfirmed') must
    /// NOT be dropped on a re-scan that fails to re-detect it — else the person is orphaned + the
    /// embedding lost. (Regression guard for the reconcile drop predicate.)
    #[test]
    fn reconcile_keeps_person_assigned_unconfirmed_face() {
        let db = Db::open_in_memory().unwrap();
        seed_image(&db, 1);
        reconcile_faces(
            &db.conn,
            1,
            "mv",
            "tag",
            0,
            &[face_at([0.1, 0.1, 0.3, 0.3])],
        )
        .unwrap();
        let id = faces_of(&db, 1)[0].0;
        // Mimic clustering: set person_id but leave status 'unconfirmed' (auto, not user-confirmed).
        let person = create_person(&db.conn, 0).unwrap();
        db.conn
            .execute(
                "UPDATE face SET person_id = ?2 WHERE id = ?1",
                params![id, person],
            )
            .unwrap();

        reconcile_faces(&db.conn, 1, "mv", "tag", 1, &[]).unwrap();
        let after = faces_of(&db, 1);
        assert_eq!(
            after.len(),
            1,
            "person-assigned face survives a zero-detection re-scan"
        );
        assert_eq!(after[0].1, Some(person), "stays linked to its person");
        assert_eq!(after[0].2, "unconfirmed");
    }
}
