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
/// One-query pre-check for the thumbnail backfill: an image's content hash plus its edit version
/// (`None` when unedited). The worker asks this for EVERY present image at startup, so two separate
/// round-trips per id meant two catalog-lock acquisitions per id — 200k of them on a 100k library,
/// all competing with foreground queries on the single connection.
pub fn thumb_precheck(
    conn: &Connection,
    image_id: i64,
) -> Result<Option<(String, Option<i64>)>, LibError> {
    Ok(conn
        .query_row(
            "SELECT i.content_hash, e.updated_at
               FROM images i LEFT JOIN edits e ON e.image_id = i.id
              WHERE i.id = ?1",
            params![image_id],
            |r| {
                // `content_hash` is 32 raw bytes on disk; every consumer (thumb cache keys included)
                // uses the hex form, exactly as `query::map_row` produces it.
                let bytes: Vec<u8> = r.get(0)?;
                let hash = match <[u8; 32]>::try_from(bytes.as_slice()) {
                    Ok(a) => core_raw::hex(&a),
                    Err(_) => String::new(),
                };
                Ok((hash, r.get::<_, Option<i64>>(1)?))
            },
        )
        .optional()?)
}

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
