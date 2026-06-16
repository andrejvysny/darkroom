//! Keywords / tags: the `keywords` + `image_keywords` tables. Keywords are flat in v1 (the
//! `parent_id` hierarchy column is unused). Names are unique case-insensitively (migration 002),
//! so [`create_or_get_keyword`] never duplicates.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeywordRow {
    pub id: i64,
    pub name: String,
    /// Number of present images carrying this keyword (0 when returned per-image).
    pub count: i64,
}

/// All keywords with present-image counts, ordered by name (for the left nav).
pub fn list_keywords(conn: &Connection) -> Result<Vec<KeywordRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT k.id, k.name,
                (SELECT COUNT(*) FROM image_keywords ik
                 JOIN images i ON i.id = ik.image_id
                 WHERE ik.keyword_id = k.id AND i.status = 'present') AS cnt
         FROM keywords k
         ORDER BY k.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(KeywordRow {
            id: r.get(0)?,
            name: r.get(1)?,
            count: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Keywords applied to a single image (count is 0 — not needed for chips).
pub fn keywords_for_image(conn: &Connection, image_id: i64) -> Result<Vec<KeywordRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT k.id, k.name FROM keywords k
         JOIN image_keywords ik ON ik.keyword_id = k.id
         WHERE ik.image_id = ?1
         ORDER BY k.name COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([image_id], |r| {
        Ok(KeywordRow {
            id: r.get(0)?,
            name: r.get(1)?,
            count: 0,
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Find a keyword by name (case-insensitive) or create it; returns its id.
pub fn create_or_get_keyword(conn: &Connection, name: &str) -> Result<i64, LibError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(LibError::Other("keyword name is empty".into()));
    }
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM keywords WHERE name = ?1 COLLATE NOCASE",
            params![trimmed],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    conn.execute("INSERT INTO keywords(name) VALUES(?1)", params![trimmed])?;
    Ok(conn.last_insert_rowid())
}

/// Create-or-get the keyword `name` and apply it to `image_id`; returns the keyword row.
pub fn add_keyword_to_image(
    conn: &Connection,
    image_id: i64,
    name: &str,
) -> Result<KeywordRow, LibError> {
    let id = create_or_get_keyword(conn, name)?;
    conn.execute(
        "INSERT OR IGNORE INTO image_keywords(image_id, keyword_id) VALUES(?1, ?2)",
        params![image_id, id],
    )?;
    let stored: String =
        conn.query_row("SELECT name FROM keywords WHERE id = ?1", params![id], |r| {
            r.get(0)
        })?;
    Ok(KeywordRow {
        id,
        name: stored,
        count: 0,
    })
}

/// Remove a keyword from an image (the keyword itself remains in the catalog).
pub fn remove_keyword_from_image(
    conn: &Connection,
    image_id: i64,
    keyword_id: i64,
) -> Result<(), LibError> {
    conn.execute(
        "DELETE FROM image_keywords WHERE image_id = ?1 AND keyword_id = ?2",
        params![image_id, keyword_id],
    )?;
    Ok(())
}

/// Delete a keyword entirely (FK cascade removes its image links).
pub fn delete_keyword(conn: &Connection, keyword_id: i64) -> Result<(), LibError> {
    conn.execute("DELETE FROM keywords WHERE id = ?1", params![keyword_id])?;
    Ok(())
}
