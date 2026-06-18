//! Non-destructive edit persistence (the `edits` table). Stores opaque params JSON keyed by image.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};

/// Saved develop params JSON for an image, if any.
pub fn get_edit(conn: &Connection, image_id: i64) -> Result<Option<String>, LibError> {
    Ok(conn
        .query_row(
            "SELECT params FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Saved develop params JSON + its `updated_at` version, if any. The version cache-busts previews.
pub fn get_edit_with_version(
    conn: &Connection,
    image_id: i64,
) -> Result<Option<(String, i64)>, LibError> {
    Ok(conn
        .query_row(
            "SELECT params, updated_at FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}

/// Upsert develop params JSON for an image.
pub fn set_edit(
    conn: &Connection,
    image_id: i64,
    process_version: i64,
    params_json: &str,
    updated_at: i64,
) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO edits(image_id, process_version, params, updated_at) VALUES(?1,?2,?3,?4)
         ON CONFLICT(image_id) DO UPDATE SET
            process_version = excluded.process_version,
            params = excluded.params,
            updated_at = excluded.updated_at",
        params![image_id, process_version, params_json, updated_at],
    )?;
    Ok(())
}
