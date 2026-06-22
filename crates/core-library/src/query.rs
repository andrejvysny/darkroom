//! Catalog queries for the Library grid + metadata panel. Injection-safe:
//! all filters are bound named params; `sort` is chosen from a fixed whitelist.

use crate::error::LibError;
use core_db::rusqlite::{named_params, Connection, Row, ToSql};
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
    /// Restrict to a capture year ("2026"), matched against `capture_date` formatted as UTC.
    pub capture_year: Option<String>,
    /// Restrict to a capture day ("2026-06-22"), matched against `capture_date` formatted as UTC.
    pub capture_date: Option<String>,
    /// Restrict to images with a detected object in this bucket ("People" | "Animals" | "Vehicles").
    pub detected_category: Option<String>,
    /// Restrict to images containing a (confirmed or suggested) face of this person.
    pub person_id: Option<i64>,
    pub search: Option<String>,
    /// "capture_desc" (default) | "capture_asc" | "filename" | "filename_desc"
    /// | "rating_desc" | "rating_asc" | "imported_desc" | "imported_asc".
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Keyset (seek) pagination cursor: the sort value (`capture_date` or `imported_at`) of the last
    /// loaded row. `None` with `cursor_id = None` = first page; `None` with `cursor_id = Some` = a
    /// cursor sitting in the NULL `capture_date` block.
    pub cursor_value: Option<i64>,
    /// Keyset cursor: `id` of the last loaded row (tie-break). Presence marks "has cursor".
    pub cursor_id: Option<i64>,
    /// Use keyset/seek pagination instead of LIMIT/OFFSET (time-based sorts only). Defaults to offset.
    pub seek: Option<bool>,
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
    /// `edits.updated_at` if the image has a develop edit — versions edit-aware previews (NULL = none).
    pub edited_at: Option<i64>,
    /// When the image was catalogued (epoch seconds). The keyset cursor for imported-date sorts and a
    /// comparator key for the frontend live sorted-merge.
    pub imported_at: i64,
}

const COLUMNS: &str = "i.id, i.content_hash, i.path, i.original_filename, i.capture_date,
    i.camera_make, i.camera_model, i.lens, i.iso, i.shutter, i.aperture, i.focal_length,
    i.width, i.height, i.orientation,
    COALESCE(rf.stars,0), COALESCE(rf.flag,'none'), rf.color_label, e.updated_at, i.imported_at";

// Joined into every row-returning query so `edited_at` is populated.
const EDIT_JOIN: &str = "LEFT JOIN edits e ON e.image_id = i.id";

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
    AND (:capture_year IS NULL OR strftime('%Y', i.capture_date, 'unixepoch') = :capture_year)
    AND (:capture_date IS NULL OR strftime('%Y-%m-%d', i.capture_date, 'unixepoch') = :capture_date)
    AND (:detected_category IS NULL
         OR EXISTS (SELECT 1 FROM image_detections d
                    WHERE d.image_id = i.id AND d.category = :detected_category)
         OR (:detected_category = 'People' AND EXISTS
              (SELECT 1 FROM image_user_labels ul
               WHERE ul.image_id = i.id AND ul.contains_person = 1))
         OR (:detected_category = 'Animals' AND EXISTS
              (SELECT 1 FROM image_user_labels ul
               WHERE ul.image_id = i.id AND ul.contains_animal = 1))
         OR (:detected_category = 'People' AND EXISTS
              (SELECT 1 FROM image_presence p
               WHERE p.image_id = i.id AND p.p_person >= :tau_person))
         OR (:detected_category = 'Animals' AND EXISTS
              (SELECT 1 FROM image_presence p
               WHERE p.image_id = i.id AND p.p_animal >= :tau_animal)))
    AND (:person_id IS NULL OR EXISTS
         (SELECT 1 FROM face fa WHERE fa.asset_id = i.id AND fa.person_id = :person_id
            AND fa.status IN ('confirmed','unconfirmed')))
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
        edited_at: r.get(18)?,
        imported_at: r.get(19)?,
    })
}

pub fn query_images(conn: &Connection, p: &QueryParams) -> Result<Vec<ImageRow>, LibError> {
    // Keyset/seek pagination (time-based sorts) — stable under concurrent inserts and O(log n) at
    // depth, unlike OFFSET. filename/rating fall through to the offset path below.
    if p.seek == Some(true) {
        if let Some(kind) = seek_kind(p.sort.as_deref()) {
            return query_images_seek(conn, p, kind, p.limit.unwrap_or(5000));
        }
    }
    let sql = format!(
        "SELECT {COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         {EDIT_JOIN}
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
            ":capture_year": p.capture_year,
            ":capture_date": p.capture_date,
            ":detected_category": p.detected_category,
            ":person_id": p.person_id,
            ":tau_person": crate::analysis::PRESENCE_TAU_PERSON,
            ":tau_animal": crate::analysis::PRESENCE_TAU_ANIMAL,
            ":search": search,
            ":limit": p.limit.unwrap_or(5000),
            ":offset": p.offset.unwrap_or(0),
        },
        map_row,
    )?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// The seek-paginated sorts. `None` ⇒ this sort uses OFFSET (filename / rating).
#[derive(Clone, Copy)]
enum SeekKind {
    CaptureDesc,
    CaptureAsc,
    ImportedDesc,
    ImportedAsc,
}

fn seek_kind(sort: Option<&str>) -> Option<SeekKind> {
    match sort {
        Some("capture_asc") => Some(SeekKind::CaptureAsc),
        Some("imported_desc") => Some(SeekKind::ImportedDesc),
        Some("imported_asc") => Some(SeekKind::ImportedAsc),
        Some("filename") | Some("filename_desc") | Some("rating_desc") | Some("rating_asc") => None,
        // default + "capture_desc"
        _ => Some(SeekKind::CaptureDesc),
    }
}

/// Run ONE seek phase: `WHERE <filters> AND (<extra_where>) ORDER BY <order> LIMIT <limit>`, appending
/// rows to `out`. Cursor params `:cv`/`:ci` are bound only when the phase references them (`use_cv` /
/// `use_ci`), so the same filter-bind set serves every phase shape. Index-safe: each phase's
/// `extra_where` reduces to a range/equality the `(status, capture_date, id)` /
/// `(status, imported_at, id)` index can seek (verified via EXPLAIN QUERY PLAN).
#[allow(clippy::too_many_arguments)]
fn run_seek_phase(
    conn: &Connection,
    p: &QueryParams,
    search: &Option<String>,
    extra_where: &str,
    order: &str,
    limit: i64,
    use_cv: bool,
    use_ci: bool,
    out: &mut Vec<ImageRow>,
) -> Result<(), LibError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         {EDIT_JOIN}
         WHERE {WHERE} AND ({extra_where})
         ORDER BY {order} LIMIT :limit"
    );
    let lim = limit;
    let cv_v = p.cursor_value.unwrap_or(0);
    let ci_v = p.cursor_id.unwrap_or(0);
    let mut binds: Vec<(&str, &dyn ToSql)> = vec![
        (":folder_id", &p.folder_id),
        (":min_stars", &p.min_stars),
        (":flag", &p.flag),
        (":color_label", &p.color_label),
        (":keyword_id", &p.keyword_id),
        (":collection_id", &p.collection_id),
        (":import_session_id", &p.import_session_id),
        (":capture_year", &p.capture_year),
        (":capture_date", &p.capture_date),
        (":detected_category", &p.detected_category),
        (":person_id", &p.person_id),
        (":tau_person", &crate::analysis::PRESENCE_TAU_PERSON),
        (":tau_animal", &crate::analysis::PRESENCE_TAU_ANIMAL),
        (":search", search),
        (":limit", &lim),
    ];
    if use_cv {
        binds.push((":cv", &cv_v));
    }
    if use_ci {
        binds.push((":ci", &ci_v));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(binds.as_slice(), map_row)?;
    for r in rows {
        out.push(r?);
    }
    Ok(())
}

/// Keyset pagination. Cursor = `(cursor_value, cursor_id)` of the last loaded row. `cursor_id = None`
/// ⇒ first page. For capture sorts the cursor may sit in the NULL block (`cursor_value = None` while
/// `cursor_id = Some`); a two-phase walk (non-NULL block, then NULL block — order swapped for ASC)
/// keeps every phase index-seekable rather than scanning the whole `present` partition (Codex-flagged
/// `OR capture_date IS NULL` regression). `imported_at` is NOT NULL ⇒ single phase.
fn query_images_seek(
    conn: &Connection,
    p: &QueryParams,
    kind: SeekKind,
    limit: i64,
) -> Result<Vec<ImageRow>, LibError> {
    let search = p.search.as_ref().map(|s| format!("%{s}%"));
    let has_cursor = p.cursor_id.is_some();
    let has_cv = p.cursor_value.is_some();
    let mut out: Vec<ImageRow> = Vec::new();

    match kind {
        SeekKind::CaptureDesc => {
            // NULLs sort last: phase 1 = non-NULL block (index range seek), phase 2 = NULL block.
            let in_null_block = has_cursor && !has_cv;
            if !in_null_block {
                let extra = if has_cv {
                    "i.capture_date IS NOT NULL AND (i.capture_date < :cv OR (i.capture_date = :cv AND i.id < :ci))"
                } else {
                    "i.capture_date IS NOT NULL"
                };
                run_seek_phase(
                    conn,
                    p,
                    &search,
                    extra,
                    "i.capture_date DESC, i.id DESC",
                    limit,
                    has_cv,
                    has_cv,
                    &mut out,
                )?;
            }
            let remaining = limit - out.len() as i64;
            if remaining > 0 {
                let extra = if in_null_block {
                    "i.capture_date IS NULL AND i.id < :ci"
                } else {
                    "i.capture_date IS NULL"
                };
                run_seek_phase(
                    conn,
                    p,
                    &search,
                    extra,
                    "i.id DESC",
                    remaining,
                    false,
                    in_null_block,
                    &mut out,
                )?;
            }
        }
        SeekKind::CaptureAsc => {
            // NULLs sort first: phase 1 = NULL block, phase 2 = non-NULL block.
            let past_null_block = has_cv; // a non-NULL cursor means we've left the NULL block
            if !past_null_block {
                let extra = if has_cursor {
                    "i.capture_date IS NULL AND i.id > :ci"
                } else {
                    "i.capture_date IS NULL"
                };
                run_seek_phase(
                    conn, p, &search, extra, "i.id ASC", limit, false, has_cursor, &mut out,
                )?;
            }
            let remaining = limit - out.len() as i64;
            if remaining > 0 {
                let extra = if has_cv {
                    "i.capture_date IS NOT NULL AND (i.capture_date > :cv OR (i.capture_date = :cv AND i.id > :ci))"
                } else {
                    "i.capture_date IS NOT NULL"
                };
                run_seek_phase(
                    conn,
                    p,
                    &search,
                    extra,
                    "i.capture_date ASC, i.id ASC",
                    remaining,
                    has_cv,
                    has_cv,
                    &mut out,
                )?;
            }
        }
        SeekKind::ImportedDesc => {
            let extra = if has_cursor {
                "(i.imported_at < :cv OR (i.imported_at = :cv AND i.id < :ci))"
            } else {
                "1=1"
            };
            run_seek_phase(
                conn,
                p,
                &search,
                extra,
                "i.imported_at DESC, i.id DESC",
                limit,
                has_cursor,
                has_cursor,
                &mut out,
            )?;
        }
        SeekKind::ImportedAsc => {
            let extra = if has_cursor {
                "(i.imported_at > :cv OR (i.imported_at = :cv AND i.id > :ci))"
            } else {
                "1=1"
            };
            run_seek_phase(
                conn,
                p,
                &search,
                extra,
                "i.imported_at ASC, i.id ASC",
                limit,
                has_cursor,
                has_cursor,
                &mut out,
            )?;
        }
    }
    Ok(out)
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DateNode {
    /// `YYYY-MM-DD` (UTC) capture day, or "Unknown" when `capture_date` is NULL.
    pub date: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DateTreeYear {
    /// `YYYY` (UTC) capture year, or "Unknown" when `capture_date` is NULL.
    pub year: String,
    pub count: i64,
    pub dates: Vec<DateNode>,
}

/// Year → day capture-date tree for the left-nav Folders section. Formats `capture_date` as UTC so
/// labels match the on-disk `YYYY/YYYY-MM-DD` routing (`core-import::date_subpath`). NULL capture
/// dates fold into an "Unknown" bucket (mirrors import's `unknown/unknown` fallback). Rows arrive
/// already grouped/ordered (year desc, day desc); we fold them into the nested shape.
pub fn date_tree(conn: &Connection) -> Result<Vec<DateTreeYear>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(strftime('%Y', capture_date, 'unixepoch'), 'Unknown')      AS y,
                COALESCE(strftime('%Y-%m-%d', capture_date, 'unixepoch'), 'Unknown') AS d,
                COUNT(*)
         FROM images WHERE status = 'present'
         GROUP BY y, d
         ORDER BY y = 'Unknown', y DESC, d = 'Unknown', d DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;

    let mut years: Vec<DateTreeYear> = Vec::new();
    for row in rows {
        let (year, date, count) = row?;
        match years.last_mut() {
            Some(y) if y.year == year => {
                y.count += count;
                y.dates.push(DateNode { date, count });
            }
            _ => years.push(DateTreeYear {
                year,
                count,
                dates: vec![DateNode { date, count }],
            }),
        }
    }
    Ok(years)
}

/// Fetch a single image row by id (for the metadata panel / develop).
pub fn image_by_id(conn: &Connection, id: i64) -> Result<Option<ImageRow>, LibError> {
    use core_db::rusqlite::OptionalExtension;
    let sql = format!(
        "SELECT {COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         {EDIT_JOIN}
         WHERE i.id = ?1"
    );
    Ok(conn.query_row(&sql, [id], map_row).optional()?)
}

/// Ids of all present images, newest capture first — the work-list for canonical thumbnail backfill.
pub fn present_image_ids(conn: &Connection) -> Result<Vec<i64>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT id FROM images WHERE status='present' ORDER BY capture_date DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
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
            ":capture_year": p.capture_year,
            ":capture_date": p.capture_date,
            ":detected_category": p.detected_category,
            ":person_id": p.person_id,
            ":tau_person": crate::analysis::PRESENCE_TAU_PERSON,
            ":tau_animal": crate::analysis::PRESENCE_TAU_ANIMAL,
            ":search": search,
        },
        |r| r.get(0),
    )?;
    Ok(n)
}
