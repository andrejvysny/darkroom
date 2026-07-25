//! RAW + JPEG/HEIF pairing: the catalog side of "the camera wrote two files for one shot".
//!
//! Both files stay ordinary `images` rows; `image_pairs` records which companion belongs to which
//! RAW. The default Library query hides linked companions (see [`crate::query`]), so a paired shot
//! occupies one grid cell while both files remain fully developable and exportable.
//!
//! Invariants enforced here (the schema only guarantees "one primary per companion"):
//! - no self-links,
//! - no chains — a companion may not itself anchor a pair, and linking to a companion re-points the
//!   link at *its* primary, so every pair stays exactly two levels deep.

use crate::error::LibError;
use crate::index::now_epoch;
use crate::query::{image_by_id, ImageRow};
use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// One image's place in a pair, for the metadata panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInfo {
    /// This image's role: `"primary"` (the RAW) or `"secondary"` (a companion JPEG/HEIF).
    pub role: String,
    /// Id of the RAW anchoring the pair (equals the queried id when `role == "primary"`).
    pub primary_id: i64,
    /// Every companion in the pair, ordered by filename — including the queried image itself when
    /// it is a secondary, so the panel can render the whole group from one call.
    pub secondaries: Vec<ImageRow>,
}

/// The primary this image is linked to, or `None` when it is not a companion.
pub fn primary_of(conn: &Connection, secondary_id: i64) -> Result<Option<i64>, LibError> {
    Ok(conn
        .query_row(
            "SELECT primary_image_id FROM image_pairs WHERE secondary_image_id = ?1",
            params![secondary_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// Ids of the companions linked to `primary_id`, ordered by filename.
pub fn secondaries_of(conn: &Connection, primary_id: i64) -> Result<Vec<i64>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT ip.secondary_image_id FROM image_pairs ip
         JOIN images i ON i.id = ip.secondary_image_id
         WHERE ip.primary_image_id = ?1
         ORDER BY i.original_filename, i.id",
    )?;
    let rows = stmt.query_map(params![primary_id], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Link `secondary_id` (a camera JPEG/HEIF) to `primary_id` (its RAW). Idempotent: re-linking an
/// already-paired companion re-points it. Returns `Ok(false)` when the link was rejected as invalid
/// (self-link, or `secondary_id` already anchors companions of its own) — a rejected link is never
/// fatal to an import, so callers can carry on.
pub fn link_pair(conn: &Connection, primary_id: i64, secondary_id: i64) -> Result<bool, LibError> {
    // Flatten: pairing onto a companion means pairing onto the RAW behind it.
    let primary_id = primary_of(conn, primary_id)?.unwrap_or(primary_id);
    if primary_id == secondary_id {
        return Ok(false);
    }
    // A companion may not anchor its own companions (no chains).
    if !secondaries_of(conn, secondary_id)?.is_empty() {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO image_pairs(secondary_image_id, primary_image_id, created_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(secondary_image_id) DO UPDATE SET
             primary_image_id = excluded.primary_image_id,
             created_at       = excluded.created_at",
        params![secondary_id, primary_id, now_epoch()],
    )?;
    Ok(true)
}

/// Break a pair. `secondary_id` returns to the grid as a standalone image; the RAW is untouched.
pub fn unlink_pair(conn: &Connection, secondary_id: i64) -> Result<(), LibError> {
    conn.execute(
        "DELETE FROM image_pairs WHERE secondary_image_id = ?1",
        params![secondary_id],
    )?;
    Ok(())
}

/// Whole-pair view for `image_id`, from either member. `None` when the image is not paired.
pub fn pair_info(conn: &Connection, image_id: i64) -> Result<Option<PairInfo>, LibError> {
    let (role, primary_id) = match primary_of(conn, image_id)? {
        Some(p) => ("secondary", p),
        None => ("primary", image_id),
    };
    let ids = secondaries_of(conn, primary_id)?;
    if ids.is_empty() {
        return Ok(None);
    }
    let mut secondaries = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(row) = image_by_id(conn, id)? {
            secondaries.push(row);
        }
    }
    Ok(Some(PairInfo {
        role: role.to_string(),
        primary_id,
        secondaries,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::Db;

    /// Minimal `images` row (the catalog needs a folder FK, so create one per call).
    fn insert_image(conn: &Connection, n: i64, name: &str) -> i64 {
        conn.execute(
            "INSERT INTO folders(path, is_watched, added_at) VALUES(?1, 1, 0)",
            params![format!("/f{n}")],
        )
        .unwrap();
        let folder = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, folder_id, original_filename, imported_at)
             VALUES (?1, 1, ?2, ?3, ?4, 0)",
            params![vec![n as u8; 32], format!("/f{n}/{name}"), folder, name],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn links_flattens_and_unlinks() {
        let db = Db::open_in_memory().unwrap();
        let conn = &db.conn;
        let raw = insert_image(conn, 1, "A.CR3");
        let jpg = insert_image(conn, 2, "A.JPG");
        let hif = insert_image(conn, 3, "A.HIF");

        assert!(link_pair(conn, raw, jpg).unwrap());
        // Pairing onto a companion flattens to the RAW behind it — never a chain.
        assert!(link_pair(conn, jpg, hif).unwrap());
        assert_eq!(primary_of(conn, hif).unwrap(), Some(raw));
        // Ordered by filename: "A.HIF" before "A.JPG".
        assert_eq!(secondaries_of(conn, raw).unwrap(), vec![hif, jpg]);

        // Rejected (not errors): self-links, and demoting a RAW that already anchors companions.
        assert!(!link_pair(conn, raw, raw).unwrap());
        let other_raw = insert_image(conn, 4, "B.CR3");
        assert!(!link_pair(conn, other_raw, raw).unwrap());
        assert!(primary_of(conn, raw).unwrap().is_none());

        // Both members see the same pair.
        let from_raw = pair_info(conn, raw).unwrap().unwrap();
        let from_jpg = pair_info(conn, jpg).unwrap().unwrap();
        assert_eq!(from_raw.role, "primary");
        assert_eq!(from_jpg.role, "secondary");
        assert_eq!(from_jpg.primary_id, raw);
        assert_eq!(from_raw.secondaries.len(), 2);

        unlink_pair(conn, jpg).unwrap();
        assert_eq!(secondaries_of(conn, raw).unwrap(), vec![hif]);
        assert!(pair_info(conn, jpg).unwrap().is_none());
    }
}
