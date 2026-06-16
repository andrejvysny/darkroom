//! Persistent app settings stored as key/value rows in `app_meta`.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};

/// Default thumbnail-cache size cap: 2 GiB.
pub const DEFAULT_THUMB_CACHE_CAP: u64 = 2 * 1024 * 1024 * 1024;

const KEY_THUMB_CACHE_CAP: &str = "thumb_cache_cap_bytes";

/// Read a raw `app_meta` value.
pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>, LibError> {
    Ok(conn
        .query_row(
            "SELECT value FROM app_meta WHERE key=?1",
            params![key],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Upsert an `app_meta` value.
pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO app_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Configured thumbnail-cache cap in bytes, or the default when unset/unparseable.
pub fn thumb_cache_cap(conn: &Connection) -> Result<u64, LibError> {
    Ok(get_meta(conn, KEY_THUMB_CACHE_CAP)?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_THUMB_CACHE_CAP))
}

/// Persist the thumbnail-cache cap in bytes.
pub fn set_thumb_cache_cap(conn: &Connection, bytes: u64) -> Result<(), LibError> {
    set_meta(conn, KEY_THUMB_CACHE_CAP, &bytes.to_string())
}
