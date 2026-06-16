//! core-dedup — duplicate detection (byte-identical + same-capture) and safe resolution.
//!
//! Detection is a `GROUP BY` over precomputed hashes (no rescan). Resolution routes files to the
//! Trash (never hard-deletes) and removes their catalog rows; the keeper is never touched.

pub mod error;

pub use error::DedupError;

use core_db::rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupImage {
    pub id: i64,
    pub content_hash: String,
    pub path: String,
    pub filename: String,
    pub file_size: i64,
    pub capture_date: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroup {
    /// Hex of the shared hash/fingerprint.
    pub key: String,
    /// "byte" or "capture".
    pub category: String,
    pub images: Vec<DupImage>,
}

fn hexs(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Query images whose `col` value is shared by >1 present image, grouped. `col` is an unqualified
/// column on `images` (`content_hash` or `capture_fingerprint`).
fn grouped_by(conn: &Connection, col: &str, category: &str) -> Result<Vec<DupGroup>, DedupError> {
    let sql = format!(
        "SELECT i.{col}, i.id, i.content_hash, i.path, i.original_filename, i.file_size, i.capture_date
         FROM images i
         WHERE i.status='present' AND i.{col} IS NOT NULL AND i.{col} IN (
             SELECT {col} FROM images WHERE status='present' AND {col} IS NOT NULL
             GROUP BY {col} HAVING COUNT(*) > 1
         )
         ORDER BY i.{col}, i.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |r| {
        let key: Vec<u8> = r.get(0)?;
        let hash: Vec<u8> = r.get(2)?;
        Ok((
            hexs(&key),
            DupImage {
                id: r.get(1)?,
                content_hash: hexs(&hash),
                path: r.get(3)?,
                filename: r.get(4)?,
                file_size: r.get(5)?,
                capture_date: r.get(6)?,
            },
        ))
    })?;

    let mut groups: Vec<DupGroup> = Vec::new();
    for row in rows {
        let (key, img) = row?;
        match groups.last_mut() {
            Some(g) if g.key == key => g.images.push(img),
            _ => groups.push(DupGroup {
                key,
                category: category.to_string(),
                images: vec![img],
            }),
        }
    }
    Ok(groups)
}

/// Byte-identical duplicates (same whole-file `content_hash`).
pub fn find_byte_identical(conn: &Connection) -> Result<Vec<DupGroup>, DedupError> {
    grouped_by(conn, "content_hash", "byte")
}

/// Same-capture duplicates (same `capture_fingerprint`; bytes may differ).
pub fn find_same_capture(conn: &Connection) -> Result<Vec<DupGroup>, DedupError> {
    grouped_by(conn, "capture_fingerprint", "capture")
}

/// Trash the given images (never the keeper) and remove their catalog rows. Returns the count trashed.
pub fn resolve(conn: &Connection, keep_id: i64, trash_ids: &[i64]) -> Result<usize, DedupError> {
    let mut trashed = 0;
    for &id in trash_ids {
        if id == keep_id {
            continue;
        }
        let path: String =
            conn.query_row("SELECT path FROM images WHERE id=?1", params![id], |r| {
                r.get(0)
            })?;
        // Route to Trash (never hard unlink). Tolerate an already-missing file.
        if std::path::Path::new(&path).exists() {
            trash::delete(&path)?;
        }
        conn.execute("DELETE FROM images WHERE id=?1", params![id])?;
        trashed += 1;
    }
    Ok(trashed)
}
