//! Per-image sidecar files (`<raw>.json`) — the durable, portable record of edit intent: develop
//! params + rating/flag/label + keywords, written next to each RAW. With sidecars the catalog
//! becomes a rebuildable CACHE: delete `catalog.db`, rescan, and the edits come back.
//!
//! - Write path: every mutating command re-emits the affected image's sidecar (best-effort, atomic).
//! - Read path: indexing a fresh/blank row (incl. a full rebuild) hydrates its sidecar back into the
//!   catalog. An explicit "rebuild from sidecars" force-applies every sidecar.
//!
//! The develop params are stored as opaque JSON (the `edits.params` blob) — core-library never
//! interprets them, so no dependency on the pipeline's `DevelopParams` shape.

use crate::error::LibError;
use crate::index::now_epoch;
use crate::query::ImageRow;
use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Sidecar on-disk schema version (bump on an incompatible shape change).
pub const SIDECAR_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SidecarRating {
    pub stars: i64,
    pub flag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sidecar {
    pub schema_version: u32,
    /// Hex content hash — lets a moved/renamed RAW be re-matched to its sidecar.
    pub content_hash: String,
    /// Opaque develop params JSON (the `edits.params` blob). `None` = no edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub develop: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_version: Option<i64>,
    pub rating: SidecarRating,
    pub keywords: Vec<String>,
    /// When this sidecar was last written (epoch seconds).
    pub updated_at: i64,
}

/// `<raw path>.json` — the sidecar location for an image's RAW path.
pub fn sidecar_path(image_path: &str) -> PathBuf {
    PathBuf::from(format!("{image_path}.json"))
}

/// Build the sidecar for one image from the current catalog state.
fn build_sidecar(conn: &Connection, row: &ImageRow) -> Result<Sidecar, LibError> {
    let edit: Option<(String, i64)> = conn
        .query_row(
            "SELECT params, process_version FROM edits WHERE image_id = ?1",
            params![row.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (develop, process_version) = match &edit {
        Some((json, pv)) => (
            serde_json::from_str::<serde_json::Value>(json).ok(),
            Some(*pv),
        ),
        None => (None, None),
    };
    let keywords = crate::keywords::keywords_for_image(conn, row.id)?
        .into_iter()
        .map(|k| k.name)
        .collect();
    Ok(Sidecar {
        schema_version: SIDECAR_SCHEMA_VERSION,
        content_hash: row.content_hash.clone(),
        develop,
        process_version,
        rating: SidecarRating {
            stars: row.stars,
            flag: row.flag.clone(),
            color_label: row.color_label.clone(),
        },
        keywords,
        updated_at: now_epoch(),
    })
}

/// Build the sidecar struct for `image_id` from the catalog (`None` if the row is gone). For tests.
pub fn gather(conn: &Connection, image_id: i64) -> Result<Option<Sidecar>, LibError> {
    match crate::query::image_by_id(conn, image_id)? {
        Some(row) => Ok(Some(build_sidecar(conn, &row)?)),
        None => Ok(None),
    }
}

fn write_to_path(path: &Path, sc: &Sidecar) -> Result<(), LibError> {
    let json = serde_json::to_vec_pretty(sc)?;
    // Atomic: write a temp sibling then rename, so a crash never leaves a half-written sidecar.
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    std::fs::write(&tmp, &json)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Gather + atomically write `image_id`'s sidecar next to its RAW. Callers treat this as best-effort
/// (log-and-continue) — a sidecar failure must never block or fail the catalog write.
pub fn write_sidecar(conn: &Connection, image_id: i64) -> Result<(), LibError> {
    let Some(row) = crate::query::image_by_id(conn, image_id)? else {
        return Ok(());
    };
    let sc = build_sidecar(conn, &row)?;
    write_to_path(&sidecar_path(&row.path), &sc)
}

/// Read + parse the sidecar for a RAW path, or `None` if absent/unreadable.
pub fn read_sidecar(image_path: &str) -> Option<Sidecar> {
    let bytes = std::fs::read(sidecar_path(image_path)).ok()?;
    serde_json::from_slice::<Sidecar>(&bytes).ok()
}

/// Apply a sidecar's state into the catalog for `image_id` (edit + rating + keywords). Additive for
/// keywords (never removes); overwrites the edit + rating.
pub fn apply(conn: &Connection, image_id: i64, sc: &Sidecar) -> Result<(), LibError> {
    let now = now_epoch();
    if let Some(dev) = &sc.develop {
        let json = serde_json::to_string(dev)?;
        crate::edits::set_edit(conn, image_id, sc.process_version.unwrap_or(1), &json, now)?;
    }
    crate::cull::set_rating(conn, image_id, sc.rating.stars)?;
    crate::cull::set_flag(conn, image_id, &sc.rating.flag)?;
    crate::cull::set_label(conn, image_id, sc.rating.color_label.as_deref())?;
    for name in &sc.keywords {
        crate::keywords::add_keyword_to_image(conn, image_id, name)?;
    }
    Ok(())
}

/// If a sidecar exists for `image_path` and the catalog row is still "blank" (no edit, default
/// rating, no keywords — e.g. just inserted, or a full catalog rebuild), apply it. Returns whether
/// it applied. Only blank rows are hydrated so an in-app change is never clobbered by a stale sidecar.
pub fn hydrate_if_blank(
    conn: &Connection,
    image_id: i64,
    image_path: &str,
) -> Result<bool, LibError> {
    let Some(sc) = read_sidecar(image_path) else {
        return Ok(false);
    };
    let Some(row) = crate::query::image_by_id(conn, image_id)? else {
        return Ok(false);
    };
    let blank = row.edited_at.is_none()
        && row.stars == 0
        && row.flag == "none"
        && row.color_label.is_none()
        && crate::keywords::keywords_for_image(conn, image_id)?.is_empty();
    if !blank {
        return Ok(false);
    }
    apply(conn, image_id, &sc)?;
    Ok(true)
}

fn present_image_paths(conn: &Connection) -> Result<Vec<(i64, String)>, LibError> {
    let mut stmt = conn.prepare("SELECT id, path FROM images WHERE status='present'")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Force-apply every present image's sidecar into the catalog (the explicit "rebuild from sidecars"
/// action). Returns the number of images hydrated.
pub fn rebuild_from_sidecars(conn: &Connection) -> Result<usize, LibError> {
    let mut n = 0;
    for (id, path) in present_image_paths(conn)? {
        if let Some(sc) = read_sidecar(&path) {
            apply(conn, id, &sc)?;
            n += 1;
        }
    }
    Ok(n)
}

/// Write sidecars for every present image (migrate an existing catalog onto sidecars). Returns count.
pub fn write_all_sidecars(conn: &Connection) -> Result<usize, LibError> {
    let mut n = 0;
    for (id, _) in present_image_paths(conn)? {
        if write_sidecar(conn, id).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::Db;

    fn insert_blank_image(conn: &Connection, hash: &[u8; 32], path: &str) -> i64 {
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
             VALUES(?1, 0, ?2, 'x.cr3', 'present', 0)",
            params![&hash[..], path],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn roundtrip_write_then_rebuild_restores_state() {
        let dir = std::env::temp_dir().join(format!("dr_sidecar_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("IMG_1.CR3");
        let raw_str = raw.display().to_string();

        let db = Db::open_in_memory().unwrap();
        let conn = &db.conn;
        let id = insert_blank_image(conn, &[7u8; 32], &raw_str);

        // Set some state, then write the sidecar.
        crate::cull::set_rating(conn, id, 4).unwrap();
        crate::cull::set_flag(conn, id, "pick").unwrap();
        crate::edits::set_edit(conn, id, 2, r#"{"exposure":1.5}"#, 100).unwrap();
        crate::keywords::add_keyword_to_image(conn, id, "sunset").unwrap();
        write_sidecar(conn, id).unwrap();
        assert!(sidecar_path(&raw_str).exists(), "sidecar file should exist");

        // Simulate "delete catalog, rescan": fresh in-memory DB, re-insert a blank row, hydrate.
        let db2 = Db::open_in_memory().unwrap();
        let conn2 = &db2.conn;
        let id2 = insert_blank_image(conn2, &[7u8; 32], &raw_str);
        let applied = hydrate_if_blank(conn2, id2, &raw_str).unwrap();
        assert!(applied, "blank row with a sidecar should hydrate");

        let row = crate::query::image_by_id(conn2, id2).unwrap().unwrap();
        assert_eq!(row.stars, 4);
        assert_eq!(row.flag, "pick");
        assert!(row.edited_at.is_some(), "develop edit should be restored");
        let kws = crate::keywords::keywords_for_image(conn2, id2).unwrap();
        assert_eq!(kws.len(), 1);
        assert_eq!(kws[0].name, "sunset");

        // A non-blank row is NOT clobbered by hydrate_if_blank.
        crate::cull::set_rating(conn2, id2, 1).unwrap();
        assert!(!hydrate_if_blank(conn2, id2, &raw_str).unwrap());
        assert_eq!(
            crate::query::image_by_id(conn2, id2)
                .unwrap()
                .unwrap()
                .stars,
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
