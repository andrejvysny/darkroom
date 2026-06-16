//! Collections: the `collections` + `collection_images` tables.
//!
//! - **Static** collections (`is_smart = 0`) hold explicit image membership.
//! - **Smart** collections (`is_smart = 1`) store a `QueryParams`-shaped JSON predicate in `query`;
//!   their "membership" is whatever that query currently matches, so counts are evaluated live via
//!   [`crate::query::count_images`] and the frontend applies the predicate as live filters.

use crate::error::LibError;
use crate::query::{count_images, QueryParams};
use core_db::rusqlite::{params, Connection};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionRow {
    pub id: i64,
    pub name: String,
    pub is_smart: bool,
    /// Predicate JSON for smart collections (a serialized `QueryParams`); None for static.
    pub query: Option<String>,
    /// Member count (static) or live-match count (smart).
    pub count: i64,
}

/// All collections with counts, ordered by name. Smart counts are evaluated against the catalog.
pub fn list_collections(conn: &Connection) -> Result<Vec<CollectionRow>, LibError> {
    let rows: Vec<(i64, String, i64, Option<String>)> = {
        let mut stmt = conn
            .prepare("SELECT id, name, is_smart, query FROM collections ORDER BY name COLLATE NOCASE")?;
        let mapped = stmt.query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        mapped.collect::<core_db::rusqlite::Result<Vec<_>>>()?
    };

    let mut out = Vec::with_capacity(rows.len());
    for (id, name, is_smart, query) in rows {
        let is_smart = is_smart != 0;
        let count = if is_smart {
            match &query {
                Some(q) => {
                    let p: QueryParams = serde_json::from_str(q).unwrap_or_default();
                    count_images(conn, &p)?
                }
                None => 0,
            }
        } else {
            conn.query_row(
                "SELECT COUNT(*) FROM collection_images ci
                 JOIN images i ON i.id = ci.image_id
                 WHERE ci.collection_id = ?1 AND i.status = 'present'",
                [id],
                |r| r.get(0),
            )?
        };
        out.push(CollectionRow {
            id,
            name,
            is_smart,
            query,
            count,
        });
    }
    Ok(out)
}

/// Static collections that contain `image_id` (for the metadata-panel membership editor).
pub fn collections_for_image(
    conn: &Connection,
    image_id: i64,
) -> Result<Vec<CollectionRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name FROM collections c
         JOIN collection_images ci ON ci.collection_id = c.id
         WHERE ci.image_id = ?1 AND c.is_smart = 0
         ORDER BY c.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([image_id], |r| {
        Ok(CollectionRow {
            id: r.get(0)?,
            name: r.get(1)?,
            is_smart: false,
            query: None,
            count: 0,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Create a collection; returns its id. `query` is the predicate JSON for smart collections.
pub fn create_collection(
    conn: &Connection,
    name: &str,
    is_smart: bool,
    query: Option<&str>,
) -> Result<i64, LibError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LibError::Other("collection name is empty".into()));
    }
    conn.execute(
        "INSERT INTO collections(name, is_smart, query) VALUES(?1, ?2, ?3)",
        params![trimmed, is_smart as i64, query],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Rename a collection.
pub fn rename_collection(conn: &Connection, id: i64, name: &str) -> Result<(), LibError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LibError::Other("collection name is empty".into()));
    }
    conn.execute(
        "UPDATE collections SET name = ?2 WHERE id = ?1",
        params![id, trimmed],
    )?;
    Ok(())
}

/// Delete a collection (FK cascade removes its membership rows).
pub fn delete_collection(conn: &Connection, id: i64) -> Result<(), LibError> {
    conn.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
    Ok(())
}

/// Add images to a static collection (idempotent); returns the number newly added.
pub fn add_images_to_collection(
    conn: &Connection,
    collection_id: i64,
    image_ids: &[i64],
) -> Result<usize, LibError> {
    let mut added = 0;
    for &image_id in image_ids {
        added += conn.execute(
            "INSERT OR IGNORE INTO collection_images(collection_id, image_id) VALUES(?1, ?2)",
            params![collection_id, image_id],
        )?;
    }
    Ok(added)
}

/// Remove images from a static collection; returns the number removed.
pub fn remove_images_from_collection(
    conn: &Connection,
    collection_id: i64,
    image_ids: &[i64],
) -> Result<usize, LibError> {
    let mut removed = 0;
    for &image_id in image_ids {
        removed += conn.execute(
            "DELETE FROM collection_images WHERE collection_id = ?1 AND image_id = ?2",
            params![collection_id, image_id],
        )?;
    }
    Ok(removed)
}
