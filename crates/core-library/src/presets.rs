//! Develop presets (the `presets` table). A preset is a SPARSE subset of `DevelopParams`: `params`
//! stores only the touched top-level fields (a camelCase JSON object) and `field_keys` is the JSON
//! array of those keys. The sparse merge + typed validation live in the `core-preset` crate / app
//! layer; this module is pure CRUD with bound params (injection-safe). Built-ins (`builtin = 1`) are
//! read-only — the app layer guards rename/delete.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

/// A preset row without its (potentially large) sparse params blob — for the list panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetSummary {
    pub id: i64,
    pub name: String,
    pub group_name: String,
    pub builtin: bool,
    pub is_favorite: bool,
    pub field_keys: Vec<String>,
    pub sort_order: i64,
}

/// A full preset row, including its sparse params JSON (opaque string to core-library).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetFull {
    pub id: i64,
    pub name: String,
    pub group_name: String,
    pub builtin: bool,
    pub is_favorite: bool,
    pub field_keys: Vec<String>,
    pub params: String,
    pub process_version: i64,
}

fn parse_keys(json: &str) -> Vec<String> {
    serde_json::from_str(json).unwrap_or_default()
}

/// All presets, ordered for the panel: built-ins first, then by group, sort order, name.
pub fn list_presets(conn: &Connection) -> Result<Vec<PresetSummary>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, group_name, builtin, is_favorite, field_keys, sort_order
         FROM presets
         ORDER BY builtin DESC, group_name COLLATE NOCASE, sort_order, name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| {
        let keys: String = r.get(5)?;
        Ok(PresetSummary {
            id: r.get(0)?,
            name: r.get(1)?,
            group_name: r.get(2)?,
            builtin: r.get::<_, i64>(3)? != 0,
            is_favorite: r.get::<_, i64>(4)? != 0,
            field_keys: parse_keys(&keys),
            sort_order: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// One preset (with its params), if it exists.
pub fn get_preset(conn: &Connection, id: i64) -> Result<Option<PresetFull>, LibError> {
    Ok(conn
        .query_row(
            "SELECT id, name, group_name, builtin, is_favorite, field_keys, params, process_version
             FROM presets WHERE id = ?1",
            params![id],
            |r| {
                let keys: String = r.get(5)?;
                Ok(PresetFull {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    group_name: r.get(2)?,
                    builtin: r.get::<_, i64>(3)? != 0,
                    is_favorite: r.get::<_, i64>(4)? != 0,
                    field_keys: parse_keys(&keys),
                    params: r.get(6)?,
                    process_version: r.get(7)?,
                })
            },
        )
        .optional()?)
}

/// Is this preset a read-only built-in?
pub fn is_builtin(conn: &Connection, id: i64) -> Result<bool, LibError> {
    Ok(conn
        .query_row(
            "SELECT builtin FROM presets WHERE id = ?1",
            params![id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
        .map(|b| b != 0)
        .unwrap_or(false))
}

/// Pick a name unique within `group_name`, appending " (2)", " (3)", … on collision.
pub fn unique_name(conn: &Connection, group_name: &str, base: &str) -> Result<String, LibError> {
    let base = base.trim();
    let base = if base.is_empty() { "Preset" } else { base };
    let exists = |name: &str| -> Result<bool, LibError> {
        Ok(conn
            .query_row(
                "SELECT 1 FROM presets WHERE group_name = ?1 AND name = ?2",
                params![group_name, name],
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
        "could not allocate a unique preset name".into(),
    ))
}

/// Insert a user preset (`builtin = 0`). Returns the new row id. `field_keys_json`/`params_json` are
/// the JSON-encoded field-key array and sparse params object. The caller resolves a unique `name`.
#[allow(clippy::too_many_arguments)]
pub fn insert_preset(
    conn: &Connection,
    name: &str,
    group_name: &str,
    is_favorite: bool,
    field_keys_json: &str,
    params_json: &str,
    process_version: i64,
    now: i64,
) -> Result<i64, LibError> {
    conn.execute(
        "INSERT INTO presets(name, group_name, builtin, is_favorite, field_keys, params,
                             process_version, sort_order, created_at, updated_at)
         VALUES(?1, ?2, 0, ?3, ?4, ?5, ?6, 0, ?7, ?7)",
        params![
            name,
            group_name,
            is_favorite as i64,
            field_keys_json,
            params_json,
            process_version,
            now
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Idempotently seed a built-in preset (`builtin = 1`). `INSERT OR IGNORE` keyed by
/// `(group_name, name)` so re-seeding never duplicates and never clobbers a user's same-named preset.
#[allow(clippy::too_many_arguments)]
pub fn seed_builtin_preset(
    conn: &Connection,
    name: &str,
    group_name: &str,
    field_keys_json: &str,
    params_json: &str,
    process_version: i64,
    now: i64,
) -> Result<(), LibError> {
    conn.execute(
        "INSERT OR IGNORE INTO presets(name, group_name, builtin, is_favorite, field_keys, params,
                                       process_version, sort_order, created_at, updated_at)
         VALUES(?1, ?2, 1, 0, ?3, ?4, ?5, 0, ?6, ?6)",
        params![
            name,
            group_name,
            field_keys_json,
            params_json,
            process_version,
            now
        ],
    )?;
    Ok(())
}

/// Update mutable metadata (any `None` leaves the column unchanged). Does not touch `params`.
pub fn update_preset(
    conn: &Connection,
    id: i64,
    name: Option<&str>,
    group_name: Option<&str>,
    is_favorite: Option<bool>,
    sort_order: Option<i64>,
    now: i64,
) -> Result<(), LibError> {
    conn.execute(
        "UPDATE presets SET
            name = COALESCE(?2, name),
            group_name = COALESCE(?3, group_name),
            is_favorite = COALESCE(?4, is_favorite),
            sort_order = COALESCE(?5, sort_order),
            updated_at = ?6
         WHERE id = ?1",
        params![
            id,
            name,
            group_name,
            is_favorite.map(|b| b as i64),
            sort_order,
            now
        ],
    )?;
    Ok(())
}

/// Delete a preset.
pub fn delete_preset(conn: &Connection, id: i64) -> Result<(), LibError> {
    conn.execute("DELETE FROM presets WHERE id = ?1", params![id])?;
    Ok(())
}
