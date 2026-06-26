//! Persistent named develop snapshots (the `develop_snapshots` table) — the saved side of the hybrid
//! edit-history model. Each snapshot is a full `DevelopParams` JSON for an image at a save-point.
//! Session undo/redo is frontend-only; these survive restart. CRUD with bound params.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// A snapshot row without its params blob — for the History panel list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotSummary {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
}

/// Snapshots for an image, newest first.
pub fn list_snapshots(conn: &Connection, image_id: i64) -> Result<Vec<SnapshotSummary>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, created_at FROM develop_snapshots
         WHERE image_id = ?1 ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([image_id], |r| {
        Ok(SnapshotSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            created_at: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// A snapshot's stored params JSON + the `process_version` it was written under (for the PV-migration
/// shim before the typed round-trip), if it exists.
pub fn get_snapshot_params(conn: &Connection, id: i64) -> Result<Option<(String, i64)>, LibError> {
    Ok(conn
        .query_row(
            "SELECT params, process_version FROM develop_snapshots WHERE id = ?1",
            params![id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}

/// Pick a snapshot name unique within an image, appending " (2)", " (3)", … on collision.
pub fn unique_snapshot_name(
    conn: &Connection,
    image_id: i64,
    base: &str,
) -> Result<String, LibError> {
    let base = base.trim();
    let base = if base.is_empty() { "Snapshot" } else { base };
    let exists = |name: &str| -> Result<bool, LibError> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM develop_snapshots WHERE image_id = ?1 AND name = ?2",
                params![image_id, name],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    };
    if !exists(base)? {
        return Ok(base.to_string());
    }
    for n in 2..1000 {
        let candidate = format!("{base} ({n})");
        if !exists(&candidate)? {
            return Ok(candidate);
        }
    }
    Err(LibError::Other(
        "could not allocate a unique snapshot name".into(),
    ))
}

/// Create a snapshot. The caller resolves a unique `name`.
pub fn create_snapshot(
    conn: &Connection,
    image_id: i64,
    name: &str,
    params_json: &str,
    process_version: i64,
    now: i64,
) -> Result<i64, LibError> {
    conn.execute(
        "INSERT INTO develop_snapshots(image_id, name, params, process_version, created_at)
         VALUES(?1, ?2, ?3, ?4, ?5)",
        params![image_id, name, params_json, process_version, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename a snapshot.
pub fn rename_snapshot(conn: &Connection, id: i64, name: &str) -> Result<(), LibError> {
    conn.execute(
        "UPDATE develop_snapshots SET name = ?2 WHERE id = ?1",
        params![id, name],
    )?;
    Ok(())
}

/// Delete a snapshot.
pub fn delete_snapshot(conn: &Connection, id: i64) -> Result<(), LibError> {
    conn.execute("DELETE FROM develop_snapshots WHERE id = ?1", params![id])?;
    Ok(())
}
