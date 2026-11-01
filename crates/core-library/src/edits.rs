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

/// Saved develop params JSON + the `process_version` it was written by, if any.
///
/// The renderer's process version, not `updated_at`: a stored edit is only interpretable together
/// with the model version that produced it, and the state machine must be able to tell "written by
/// a newer build" apart from "corrupt". Read-only; nothing here advances the row's revision.
pub fn get_edit_with_process_version(
    conn: &Connection,
    image_id: i64,
) -> Result<Option<(String, i64)>, LibError> {
    Ok(conn
        .query_row(
            "SELECT params, process_version FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()?)
}

/// Saved develop params JSON + its monotonic `updated_at` revision, if any. The revision cache-busts
/// previews; it is not the renderer's process version. Render caches must include that separately.
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

/// Upsert develop params JSON for an image. `updated_at` is also the cache revision, so ensure it
/// strictly increases even for saves in the same clock tick or after a wall-clock adjustment.
/// Readers must use the stored revision rather than assuming it equals the supplied timestamp.
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
            updated_at = MAX(excluded.updated_at, edits.updated_at + 1)",
        params![image_id, process_version, params_json, updated_at],
    )?;
    Ok(())
}

/// Save an edit AND the durable intent to mirror it into the sidecar beside the RAW, in one
/// transaction — the point of migration 026. Returns the new durable `revision`.
///
/// Same columns and the same `updated_at` monotonicity rule as [`set_edit`], plus
/// `revision = revision + 1` (1 for a brand-new row): a save counter that cannot repeat, whatever
/// the wall clock does, so a sidecar write can be pinned to the exact params it wrote. The
/// `sidecar_outbox` row (one per image) is then upserted to that revision with the image's current
/// path + content hash, and its attempt counters reset — a fresh save is a fresh start, and it
/// replaces whatever older intent was still queued.
///
/// A missing image row is an error and writes nothing: an edit whose source file cannot be named has
/// no sidecar to mirror, and committing it would leave the catalog claiming an edit that no file
/// will ever carry.
///
/// The catalog side only — the host performs the actual write and reports it via
/// [`crate::outbox::complete_outbox`] / [`crate::outbox::fail_outbox`].
pub fn save_edit_with_intent(
    conn: &Connection,
    image_id: i64,
    process_version: i64,
    params_json: &str,
    updated_at: i64,
) -> Result<i64, LibError> {
    let identity = conn
        .query_row(
            "SELECT path, content_hash FROM images WHERE id = ?1",
            params![image_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let (path, content_hash) = match identity {
        Some(identity) => identity,
        None => return Err(LibError::Other(format!("image {image_id} does not exist"))),
    };

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO edits(image_id, process_version, params, updated_at, revision)
         VALUES(?1,?2,?3,?4,1)
         ON CONFLICT(image_id) DO UPDATE SET
            process_version = excluded.process_version,
            params = excluded.params,
            updated_at = MAX(excluded.updated_at, edits.updated_at + 1),
            revision = edits.revision + 1",
        params![image_id, process_version, params_json, updated_at],
    )?;
    let revision: i64 = tx.query_row(
        "SELECT revision FROM edits WHERE image_id = ?1",
        params![image_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO sidecar_outbox(image_id, edit_revision, content_hash, path, enqueued_at, attempts, last_error)
         VALUES(?1,?2,?3,?4,?5,0,NULL)
         ON CONFLICT(image_id) DO UPDATE SET
            edit_revision = excluded.edit_revision,
            content_hash = excluded.content_hash,
            path = excluded.path,
            enqueued_at = excluded.enqueued_at,
            attempts = 0,
            last_error = NULL",
        params![image_id, revision, content_hash, path, crate::index::now_epoch()],
    )?;
    tx.commit()?;
    Ok(revision)
}
