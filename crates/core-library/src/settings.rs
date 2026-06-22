//! Persistent app settings stored as key/value rows in `app_meta`.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};

/// Default cap for the EVICTABLE render cache (camera placeholders + edited variants + display-sharp
/// previews): 8 GiB. Durable canonical `_dev` thumbnails sit on top of this and are never evicted.
pub const DEFAULT_THUMB_CACHE_CAP: u64 = 8 * 1024 * 1024 * 1024;

const KEY_THUMB_CACHE_CAP: &str = "thumb_cache_cap_bytes";

/// Display-sharp preview longest edge (px). The loupe / develop first-paint show this tier. `0` =
/// unset — the frontend picks a default from the display resolution on first launch and persists it.
pub const PREVIEW_EDGE_MIN: u32 = 2560;
pub const PREVIEW_EDGE_MAX: u32 = 4096;
const KEY_PREVIEW_EDGE: &str = "preview_edge";

/// MegaDetector letterbox input size (px). 1280 = best recall, 640 = ~4× faster.
pub const DEFAULT_ANIMAL_DETECTOR_SIZE: u32 = 1280;
const KEY_ANIMAL_DETECTOR_SIZE: &str = "animal_detector_size";

/// User-configured library root: where copy/move imports file photos (under `YYYY/YYYY-MM-DD/`).
const KEY_LIBRARY_ROOT: &str = "library_root";

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

/// Configured preview longest-edge in px, or `0` when unset (frontend hasn't picked a default yet).
pub fn preview_edge(conn: &Connection) -> Result<u32, LibError> {
    Ok(get_meta(conn, KEY_PREVIEW_EDGE)?
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0))
}

/// Persist the preview longest-edge (clamped to `[PREVIEW_EDGE_MIN, PREVIEW_EDGE_MAX]`).
pub fn set_preview_edge(conn: &Connection, edge: u32) -> Result<(), LibError> {
    let edge = edge.clamp(PREVIEW_EDGE_MIN, PREVIEW_EDGE_MAX);
    set_meta(conn, KEY_PREVIEW_EDGE, &edge.to_string())
}

/// Configured MegaDetector input size (640 or 1280), or the default when unset/invalid.
pub fn animal_detector_size(conn: &Connection) -> Result<u32, LibError> {
    Ok(get_meta(conn, KEY_ANIMAL_DETECTOR_SIZE)?
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|&s| s == 640 || s == 1280)
        .unwrap_or(DEFAULT_ANIMAL_DETECTOR_SIZE))
}

/// Persist the MegaDetector input size (clamped to 640/1280).
pub fn set_animal_detector_size(conn: &Connection, size: u32) -> Result<(), LibError> {
    let size = if size <= 640 { 640 } else { 1280 };
    set_meta(conn, KEY_ANIMAL_DETECTOR_SIZE, &size.to_string())
}

/// User-configured library root (the copy/move import destination), if one has been set.
pub fn library_root(conn: &Connection) -> Result<Option<String>, LibError> {
    Ok(get_meta(conn, KEY_LIBRARY_ROOT)?.filter(|s| !s.is_empty()))
}

/// Persist the library root (the copy/move import destination).
pub fn set_library_root(conn: &Connection, path: &str) -> Result<(), LibError> {
    set_meta(conn, KEY_LIBRARY_ROOT, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::Db;

    #[test]
    fn library_root_round_trips_and_defaults_none() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(library_root(&db.conn).unwrap(), None);
        set_library_root(&db.conn, "/Volumes/Photos/Library").unwrap();
        assert_eq!(
            library_root(&db.conn).unwrap().as_deref(),
            Some("/Volumes/Photos/Library")
        );
        // An empty stored value reads back as "unset".
        set_library_root(&db.conn, "").unwrap();
        assert_eq!(library_root(&db.conn).unwrap(), None);
    }
}
