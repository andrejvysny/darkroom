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

/// Whether the face stage (SCRFD + ArcFace) participates in the unified AI scan. Defaults ON, so once
/// the (non-commercial, ~190 MB) face models are downloaded faces are found automatically as part of
/// the scan; a user can turn it off here. The models themselves are NEVER fetched implicitly — that
/// stays an explicit action.
const KEY_FACE_STAGE_ENABLED: &str = "face_stage_enabled";
/// JSON array of the scan modal's last stage selection.
const KEY_SCAN_STAGES: &str = "scan_stages";

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

/// Interactive-segmentation (AI masking) quality tier: `"realtime"` (MobileSAM, the only functional
/// tier today) | `"balanced"` | `"max"` (reserved for SAM2-tiny / SAM-HQ once their single-image ONNX
/// exports are validated). Default `"realtime"`.
const KEY_MASK_AI_TIER: &str = "mask_ai_tier";

/// Configured AI-masking tier tag, or `"realtime"` when unset/invalid.
pub fn mask_ai_tier(conn: &Connection) -> Result<String, LibError> {
    Ok(get_meta(conn, KEY_MASK_AI_TIER)?
        .filter(|v| matches!(v.as_str(), "realtime" | "balanced" | "max"))
        .unwrap_or_else(|| "realtime".to_string()))
}

/// Persist the AI-masking tier (unknown tags coerce to `"realtime"`).
pub fn set_mask_ai_tier(conn: &Connection, tier: &str) -> Result<(), LibError> {
    let tier = match tier {
        "balanced" => "balanced",
        "max" => "max",
        _ => "realtime",
    };
    set_meta(conn, KEY_MASK_AI_TIER, tier)
}

/// Whether the face stage runs in the unified scan (defaults ON — see [`KEY_FACE_STAGE_ENABLED`]).
pub fn face_stage_enabled(conn: &Connection) -> Result<bool, LibError> {
    Ok(get_meta(conn, KEY_FACE_STAGE_ENABLED)?
        .map(|v| v != "0")
        .unwrap_or(true))
}

/// Enable/disable the face stage in the unified scan.
pub fn set_face_stage_enabled(conn: &Connection, enabled: bool) -> Result<(), LibError> {
    set_meta(
        conn,
        KEY_FACE_STAGE_ENABLED,
        if enabled { "1" } else { "0" },
    )
}

/// The stage ticks the scan modal last ran with, so it reopens as the user left it.
///
/// Stored as a JSON array of [`crate::analysis::StageId`]. Unknown ids in a stored value are
/// **dropped on read** rather than trusted — a downgrade, or a hand-edited catalog, must not be able
/// to smuggle an unrecognised stage into a run. An absent/empty/corrupt value falls back to
/// `default_scan_stages`, which honours the legacy global face toggle.
pub fn scan_stages(conn: &Connection) -> Result<Vec<crate::analysis::StageId>, LibError> {
    use crate::analysis::StageId;
    let raw = get_meta(conn, KEY_SCAN_STAGES)?;
    let parsed: Option<Vec<StageId>> = raw
        .as_deref()
        .and_then(|s| serde_json::from_str::<Vec<StageId>>(s).ok());
    match parsed {
        Some(v) if !v.is_empty() => {
            // Dedup while preserving the canonical UI order.
            Ok(StageId::ALL
                .iter()
                .copied()
                .filter(|s| v.contains(s))
                .collect())
        }
        _ => default_scan_stages(conn),
    }
}

/// First-run default: everything the user hasn't opted out of. Faces follow the existing global
/// toggle so an installation that had faces switched off doesn't silently start scanning them.
pub fn default_scan_stages(conn: &Connection) -> Result<Vec<crate::analysis::StageId>, LibError> {
    use crate::analysis::StageId;
    let faces = face_stage_enabled(conn)?;
    Ok(StageId::ALL
        .iter()
        .copied()
        .filter(|s| match s {
            StageId::Faces => faces,
            // Panorama detection is library-wide and comparatively slow; opt-in, not default-on.
            StageId::Panoramas => false,
            _ => true,
        })
        .collect())
}

/// Remember the modal's stage ticks.
pub fn set_scan_stages(
    conn: &Connection,
    stages: &[crate::analysis::StageId],
) -> Result<(), LibError> {
    set_meta(conn, KEY_SCAN_STAGES, &serde_json::to_string(stages)?)
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
    fn scan_stages_round_trip_and_reject_junk() {
        use crate::analysis::StageId;
        let db = Db::open_in_memory().unwrap();

        // Default: everything except panorama (opt-in) — faces follow the legacy global toggle.
        let d = scan_stages(&db.conn).unwrap();
        assert!(d.contains(&StageId::Objects) && d.contains(&StageId::Captions));
        assert!(d.contains(&StageId::Faces));
        assert!(d.contains(&StageId::Embeddings));
        assert!(!d.contains(&StageId::Panoramas));

        set_face_stage_enabled(&db.conn, false).unwrap();
        assert!(
            !scan_stages(&db.conn).unwrap().contains(&StageId::Faces),
            "an installation with faces switched off must not silently start scanning them"
        );

        set_scan_stages(&db.conn, &[StageId::Captions, StageId::Panoramas]).unwrap();
        assert_eq!(
            scan_stages(&db.conn).unwrap(),
            vec![StageId::Captions, StageId::Panoramas]
        );

        // A hand-edited / downgraded catalog must not smuggle an unknown stage into a run.
        set_meta(&db.conn, KEY_SCAN_STAGES, r#"["objects","totallyBogus"]"#).unwrap();
        assert_eq!(
            scan_stages(&db.conn).unwrap(),
            default_scan_stages(&db.conn).unwrap(),
            "an unparseable selection falls back to the default, never to a partial one"
        );
        set_meta(&db.conn, KEY_SCAN_STAGES, "[]").unwrap();
        assert_eq!(
            scan_stages(&db.conn).unwrap(),
            default_scan_stages(&db.conn).unwrap()
        );
        set_meta(&db.conn, KEY_SCAN_STAGES, "not json at all").unwrap();
        assert_eq!(
            scan_stages(&db.conn).unwrap(),
            default_scan_stages(&db.conn).unwrap()
        );
    }

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
