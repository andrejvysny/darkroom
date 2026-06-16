//! Catalog queries for the Library grid + metadata panel. Injection-safe:
//! all filters are bound named params; `sort` is chosen from a fixed whitelist.

use crate::error::LibError;
use core_db::rusqlite::{named_params, Connection, Row};
use core_raw::hex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryParams {
    pub folder_id: Option<i64>,
    pub min_stars: Option<i64>,
    /// Exact flag match: "pick" | "reject" | "none".
    pub flag: Option<String>,
    /// Exact color-label match (e.g. "red"); use the sentinel "__none__" to match unlabeled.
    pub color_label: Option<String>,
    /// Restrict to images tagged with this keyword id.
    pub keyword_id: Option<i64>,
    /// Restrict to members of this (static) collection id.
    pub collection_id: Option<i64>,
    /// Restrict to images added by this import session id.
    pub import_session_id: Option<i64>,
    pub search: Option<String>,
    /// "capture_desc" (default) | "capture_asc" | "filename" | "filename_desc"
    /// | "rating_desc" | "rating_asc" | "imported_desc" | "imported_asc".
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRow {
    pub id: i64,
    pub content_hash: String,
    pub path: String,
    pub filename: String,
    pub capture_date: Option<i64>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub shutter: Option<String>,
    pub aperture: Option<f64>,
    pub focal_length: Option<f64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub orientation: Option<i64>,
    pub stars: i64,
    pub flag: String,
    pub color_label: Option<String>,
}

const COLUMNS: &str = "i.id, i.content_hash, i.path, i.original_filename, i.capture_date,
    i.camera_make, i.camera_model, i.lens, i.iso, i.shutter, i.aperture, i.focal_length,
    i.width, i.height, i.orientation,
    COALESCE(rf.stars,0), COALESCE(rf.flag,'none'), rf.color_label";

// All filter dimensions are bound named params; NULL no-ops each clause. Keyword/collection
// membership use EXISTS subqueries so there is no row duplication and the static SELECT stays
// simple. The keyword-name search branch sits inside the `:search IS NULL OR …` group, so it is
// never evaluated on unfiltered queries.
const WHERE: &str = "i.status = 'present'
    AND (:folder_id IS NULL OR i.folder_id = :folder_id)
    AND (:min_stars IS NULL OR COALESCE(rf.stars,0) >= :min_stars)
    AND (:flag IS NULL OR COALESCE(rf.flag,'none') = :flag)
    AND (:color_label IS NULL
         OR (:color_label = '__none__' AND rf.color_label IS NULL)
         OR rf.color_label = :color_label)
    AND (:keyword_id IS NULL OR EXISTS
         (SELECT 1 FROM image_keywords ik WHERE ik.image_id = i.id AND ik.keyword_id = :keyword_id))
    AND (:collection_id IS NULL OR EXISTS
         (SELECT 1 FROM collection_images ci WHERE ci.image_id = i.id AND ci.collection_id = :collection_id))
    AND (:import_session_id IS NULL OR i.import_session_id = :import_session_id)
    AND (:search IS NULL OR i.original_filename LIKE :search
                         OR i.camera_model LIKE :search
                         OR i.lens LIKE :search
                         OR EXISTS (SELECT 1 FROM image_keywords ik
                                    JOIN keywords k ON k.id = ik.keyword_id
                                    WHERE ik.image_id = i.id AND k.name LIKE :search))";

fn sort_sql(sort: Option<&str>) -> &'static str {
    match sort {
        Some("capture_asc") => "i.capture_date ASC, i.id ASC",
        Some("filename") => "i.original_filename ASC, i.id ASC",
        Some("filename_desc") => "i.original_filename DESC, i.id DESC",
        Some("rating_desc") => "COALESCE(rf.stars,0) DESC, i.capture_date DESC, i.id DESC",
        Some("rating_asc") => "COALESCE(rf.stars,0) ASC, i.capture_date DESC, i.id DESC",
        Some("imported_desc") => "i.imported_at DESC, i.id DESC",
        Some("imported_asc") => "i.imported_at ASC, i.id ASC",
        _ => "i.capture_date DESC, i.id DESC",
    }
}

fn map_row(r: &Row<'_>) -> core_db::rusqlite::Result<ImageRow> {
    let hash_bytes: Vec<u8> = r.get(1)?;
    let content_hash = if hash_bytes.len() == 32 {
        let mut a = [0u8; 32];
        a.copy_from_slice(&hash_bytes);
        hex(&a)
    } else {
        String::new()
    };
    Ok(ImageRow {
        id: r.get(0)?,
        content_hash,
        path: r.get(2)?,
        filename: r.get(3)?,
        capture_date: r.get(4)?,
        camera_make: r.get(5)?,
        camera_model: r.get(6)?,
        lens: r.get(7)?,
        iso: r.get(8)?,
        shutter: r.get(9)?,
        aperture: r.get(10)?,
        focal_length: r.get(11)?,
        width: r.get(12)?,
        height: r.get(13)?,
        orientation: r.get(14)?,
        stars: r.get(15)?,
        flag: r.get(16)?,
        color_label: r.get(17)?,
    })
}

pub fn query_images(conn: &Connection, p: &QueryParams) -> Result<Vec<ImageRow>, LibError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         WHERE {WHERE}
         ORDER BY {} LIMIT :limit OFFSET :offset",
        sort_sql(p.sort.as_deref())
    );
    let search = p.search.as_ref().map(|s| format!("%{s}%"));
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        named_params! {
            ":folder_id": p.folder_id,
            ":min_stars": p.min_stars,
            ":flag": p.flag,
            ":color_label": p.color_label,
            ":keyword_id": p.keyword_id,
            ":collection_id": p.collection_id,
            ":import_session_id": p.import_session_id,
            ":search": search,
            ":limit": p.limit.unwrap_or(5000),
            ":offset": p.offset.unwrap_or(0),
        },
        map_row,
    )?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub id: i64,
    pub path: String,
    pub count: i64,
}

/// Watched folders with present-image counts (for the left nav).
pub fn list_folders(conn: &Connection) -> Result<Vec<FolderRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, COUNT(i.id)
         FROM folders f
         LEFT JOIN images i ON i.folder_id = f.id AND i.status = 'present'
         GROUP BY f.id, f.path
         ORDER BY f.path",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(FolderRow {
            id: r.get(0)?,
            path: r.get(1)?,
            count: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Fetch a single image row by id (for the metadata panel / develop).
pub fn image_by_id(conn: &Connection, id: i64) -> Result<Option<ImageRow>, LibError> {
    use core_db::rusqlite::OptionalExtension;
    let sql = format!(
        "SELECT {COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         WHERE i.id = ?1"
    );
    Ok(conn.query_row(&sql, [id], map_row).optional()?)
}

pub fn count_images(conn: &Connection, p: &QueryParams) -> Result<i64, LibError> {
    let sql = format!(
        "SELECT COUNT(*) FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         WHERE {WHERE}"
    );
    let search = p.search.as_ref().map(|s| format!("%{s}%"));
    let n = conn.query_row(
        &sql,
        named_params! {
            ":folder_id": p.folder_id,
            ":min_stars": p.min_stars,
            ":flag": p.flag,
            ":color_label": p.color_label,
            ":keyword_id": p.keyword_id,
            ":collection_id": p.collection_id,
            ":import_session_id": p.import_session_id,
            ":search": search,
        },
        |r| r.get(0),
    )?;
    Ok(n)
}
