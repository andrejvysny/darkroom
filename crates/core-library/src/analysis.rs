//! Persistence for AI scan-analysis results (object detection + captioning).
//!
//! Storage is generic over analyzers: the canonical `analysis_results` row (image × analyzer ×
//! model_version) holds the JSON payload; known analyzer ids are also projected into the
//! denormalized `image_detections` / `image_captions` tables for fast filtering and display.
//! Kept free of any ML/ort dependency — it reads the payload JSON directly.

use std::collections::HashSet;

use core_db::rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde::Serialize;

use crate::error::LibError;

pub const OBJECT_DETECTION_ID: &str = "object_detection";
pub const ANIMAL_DETECTION_ID: &str = "animal_detection";
pub const CAPTION_ID: &str = "caption";
pub const PRESENCE_ID: &str = "presence_probe";

/// Facet/filter fusion threshold for the MobileCLIP presence probe. Set to 1.1 (> any probability) to
/// **disable** OR-fusion — the probe ships **advisory-only** (RightInfo readout via `presence_for_image`),
/// not wired into the People/Animals nav counts or library filter. Rationale: honest group-aware CV
/// showed the probe overfits the library's ~19 distinct scenes (fusing hurts animal precision and is
/// only marginal for person). Re-enable by setting these to the trained max-F1 `tau` once the probe is
/// retrained on scene-diverse labels.
pub const PRESENCE_TAU_PERSON: f64 = 1.1;
pub const PRESENCE_TAU_ANIMAL: f64 = 1.1;

/// One analyzer result to persist (mirror of the ML crate's `AnalysisRecord`, kept local so
/// `core-library` doesn't depend on `core-analyze`/ort).
pub struct AnalysisInput {
    pub analyzer_id: String,
    pub model_version: String,
    pub payload: serde_json::Value,
}

/// All `(image_id, analyzer_id, model_version)` triples already stored — drives version-gated
/// incremental skip in the analysis pass.
pub fn existing_analysis(conn: &Connection) -> Result<HashSet<(i64, String, String)>, LibError> {
    let mut stmt =
        conn.prepare("SELECT image_id, analyzer_id, model_version FROM analysis_results")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    Ok(rows.filter_map(Result::ok).collect())
}

/// Persist one image's analyzer records (idempotent). Writes `analysis_results` plus the
/// denormalized projection tables. MUST be called inside a transaction by the caller.
pub fn insert_analysis(
    conn: &Connection,
    image_id: i64,
    ran_at: i64,
    records: &[AnalysisInput],
) -> Result<(), LibError> {
    for rec in records {
        let payload = serde_json::to_string(&rec.payload)?;
        conn.execute(
            "INSERT OR REPLACE INTO analysis_results
               (image_id, analyzer_id, model_version, ran_at, status, payload)
             VALUES (?1, ?2, ?3, ?4, 'ok', ?5)",
            params![
                image_id,
                rec.analyzer_id,
                rec.model_version,
                ran_at,
                payload
            ],
        )?;
        // Each detector owns disjoint categories; scope the delete so two detectors don't clobber
        // each other's rows for the same image. D-FINE → People/Vehicles; MegaDetector → Animals.
        match rec.analyzer_id.as_str() {
            OBJECT_DETECTION_ID => {
                project_detections(conn, image_id, rec, &["People", "Vehicles"])?
            }
            ANIMAL_DETECTION_ID => project_detections(conn, image_id, rec, &["Animals"])?,
            CAPTION_ID => project_caption(conn, image_id, ran_at, rec)?,
            PRESENCE_ID => project_presence(conn, image_id, rec)?,
            _ => {}
        }
    }
    Ok(())
}

fn project_detections(
    conn: &Connection,
    image_id: i64,
    rec: &AnalysisInput,
    owned_categories: &[&str],
) -> Result<(), LibError> {
    for cat in owned_categories {
        conn.execute(
            "DELETE FROM image_detections WHERE image_id = ?1 AND category = ?2",
            params![image_id, cat],
        )?;
    }
    let Some(arr) = rec.payload.get("detections").and_then(|v| v.as_array()) else {
        return Ok(());
    };
    let mut stmt = conn.prepare(
        "INSERT INTO image_detections
           (image_id, label, category, confidence, bbox_x0, bbox_y0, bbox_x1, bbox_y1, model_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for d in arr {
        let label = d.get("label").and_then(|v| v.as_str()).unwrap_or_default();
        let category = d
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let conf = d.get("confidence").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let bb = d.get("bbox").and_then(|v| v.as_array());
        let g = |i: usize| {
            bb.and_then(|a| a.get(i))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
        };
        stmt.execute(params![
            image_id,
            label,
            category,
            conf,
            g(0),
            g(1),
            g(2),
            g(3),
            rec.model_version
        ])?;
    }
    Ok(())
}

fn project_caption(
    conn: &Connection,
    image_id: i64,
    ran_at: i64,
    rec: &AnalysisInput,
) -> Result<(), LibError> {
    let caption = rec
        .payload
        .get("caption")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let empty = serde_json::Value::Array(Vec::new());
    let keywords = serde_json::to_string(rec.payload.get("keywords").unwrap_or(&empty))?;
    conn.execute(
        "INSERT OR REPLACE INTO image_captions
           (image_id, caption, keywords, model_version, generated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![image_id, caption, keywords, rec.model_version, ran_at],
    )?;
    Ok(())
}

fn project_presence(conn: &Connection, image_id: i64, rec: &AnalysisInput) -> Result<(), LibError> {
    let p_person = rec
        .payload
        .get("p_person")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let p_animal = rec
        .payload
        .get("p_animal")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    conn.execute(
        "INSERT OR REPLACE INTO image_presence (image_id, p_person, p_animal, model_version)
         VALUES (?1, ?2, ?3, ?4)",
        params![image_id, p_person, p_animal, rec.model_version],
    )?;
    Ok(())
}

/// A present image to (potentially) analyze.
pub struct AnalyzeTarget {
    pub id: i64,
    pub path: String,
    pub content_hash_hex: String,
}

/// All present images (id, path, content-hash hex) in id order — the analysis pass filters these
/// against [`existing_analysis`].
pub fn present_images(conn: &Connection) -> Result<Vec<AnalyzeTarget>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, content_hash FROM images WHERE status = 'present' ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, path, hb) = row?;
        let content_hash_hex = if hb.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&hb);
            core_raw::hex(&a)
        } else {
            String::new()
        };
        out.push(AnalyzeTarget {
            id,
            path,
            content_hash_hex,
        });
    }
    Ok(out)
}

/// One AI-scan stage to test for staleness: its analyzer id + the current model version.
#[derive(Debug, Clone, Copy)]
pub struct StageSpec {
    pub analyzer_id: &'static str,
    pub model_version: &'static str,
}

/// A present image with a per-stage staleness mask (aligned to the `stages` slice passed to
/// [`stale_targets`]). `stale[i] == true` means stage `i` has no `status='ok'` marker at its current
/// version for this image — i.e. it must run. A missing OR `status='error'` marker both count as
/// stale, so failed stages retry instead of being treated as done.
pub struct StaleTarget {
    pub id: i64,
    pub path: String,
    pub content_hash_hex: String,
    pub stale: Vec<bool>,
}

/// Keyset-paginated dirty-stage scan for the unified AI pass. Returns present images with
/// `id > cursor` in ascending id order where AT LEAST ONE stage is stale, at most `limit` rows, each
/// tagged with which stages are stale. One `LEFT JOIN` per stage onto `analysis_results`, keyed on
/// `(analyzer_id, model_version, status='ok')`. Never materializes the whole library — the caller
/// loops, advancing `cursor` to the last returned id, until a page comes back empty.
///
/// Per-stage (not all-or-nothing): bumping one stage's version re-runs only that stage, so a caption
/// change never re-runs the ~950 ms/image animal detector across the library. Aliases (`j{k}`) and
/// the column count derive from `stages.len()` (internal, trusted); all analyzer ids / versions are
/// bound parameters, so the dynamic SQL is injection-safe.
pub fn stale_targets(
    conn: &Connection,
    stages: &[StageSpec],
    cursor: i64,
    limit: i64,
) -> Result<Vec<StaleTarget>, LibError> {
    if stages.is_empty() {
        return Ok(Vec::new());
    }
    let n = stages.len();
    let mut sql = String::from("SELECT i.id, i.path, i.content_hash");
    for k in 0..n {
        sql.push_str(&format!(", (j{k}.image_id IS NULL)"));
    }
    sql.push_str(" FROM images i");
    for k in 0..n {
        sql.push_str(&format!(
            " LEFT JOIN analysis_results j{k} ON j{k}.image_id = i.id \
             AND j{k}.analyzer_id = ? AND j{k}.model_version = ? AND j{k}.status = 'ok'"
        ));
    }
    sql.push_str(" WHERE i.status = 'present' AND i.id > ? AND (");
    for k in 0..n {
        if k > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(&format!("j{k}.image_id IS NULL"));
    }
    sql.push_str(") ORDER BY i.id LIMIT ?");

    // Params appear in SQL order: per-stage (analyzer_id, model_version) inside the JOINs, then the
    // cursor, then the limit.
    let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(n * 2 + 2);
    for s in stages {
        binds.push(&s.analyzer_id);
        binds.push(&s.model_version);
    }
    binds.push(&cursor);
    binds.push(&limit);

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(binds.as_slice(), |r| {
        let id = r.get::<_, i64>(0)?;
        let path = r.get::<_, String>(1)?;
        let hb = r.get::<_, Vec<u8>>(2)?;
        let mut stale = Vec::with_capacity(n);
        for k in 0..n {
            stale.push(r.get::<_, bool>(3 + k)?);
        }
        Ok((id, path, hb, stale))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, path, hb, stale) = row?;
        let content_hash_hex = if hb.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&hb);
            core_raw::hex(&a)
        } else {
            String::new()
        };
        out.push(StaleTarget {
            id,
            path,
            content_hash_hex,
            stale,
        });
    }
    Ok(out)
}

/// COUNT of present images with ≥1 stale stage — the denominator for scan progress. Same join shape
/// as [`stale_targets`], without pagination.
pub fn stale_count(conn: &Connection, stages: &[StageSpec]) -> Result<i64, LibError> {
    if stages.is_empty() {
        return Ok(0);
    }
    let n = stages.len();
    let mut sql = String::from("SELECT COUNT(*) FROM images i");
    for k in 0..n {
        sql.push_str(&format!(
            " LEFT JOIN analysis_results j{k} ON j{k}.image_id = i.id \
             AND j{k}.analyzer_id = ? AND j{k}.model_version = ? AND j{k}.status = 'ok'"
        ));
    }
    sql.push_str(" WHERE i.status = 'present' AND (");
    for k in 0..n {
        if k > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(&format!("j{k}.image_id IS NULL"));
    }
    sql.push(')');
    let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(n * 2);
    for s in stages {
        binds.push(&s.analyzer_id);
        binds.push(&s.model_version);
    }
    Ok(conn.query_row(&sql, binds.as_slice(), |r| r.get(0))?)
}

/// Keyset page of present images (`id > cursor`, ascending) for a FORCED full re-scan that ignores
/// staleness. Mirrors [`stale_targets`] pagination but returns every present image — the caller treats
/// all stages as stale. Walks `idx_images_status_id` directly.
pub fn present_targets_after(
    conn: &Connection,
    cursor: i64,
    limit: i64,
) -> Result<Vec<AnalyzeTarget>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT id, path, content_hash FROM images
          WHERE status = 'present' AND id > ?1 ORDER BY id LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![cursor, limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, path, hb) = row?;
        let content_hash_hex = if hb.len() == 32 {
            let mut a = [0u8; 32];
            a.copy_from_slice(&hb);
            core_raw::hex(&a)
        } else {
            String::new()
        };
        out.push(AnalyzeTarget {
            id,
            path,
            content_hash_hex,
        });
    }
    Ok(out)
}

/// One labeled image for the eval / training harnesses: catalog path + tri-state ground-truth.
/// `person`/`animal` are `None` when that field is unlabeled (NULL) — callers MUST exclude `None`
/// from that category's metrics; never treat unlabeled as a negative.
#[derive(Debug, Clone)]
pub struct LabeledImage {
    pub id: i64,
    pub path: String,
    pub person: Option<bool>,
    pub animal: Option<bool>,
}

/// All present images that carry a manual label, joined to `images.path`, in id order. Reuses the
/// `present`-status filter from [`present_images`] and the tri-state decode from [`user_labels`].
pub fn labeled_images(conn: &Connection) -> Result<Vec<LabeledImage>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.path, ul.contains_person, ul.contains_animal
           FROM image_user_labels ul JOIN images i ON i.id = ul.image_id
          WHERE i.status = 'present' ORDER BY i.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(LabeledImage {
            id: r.get::<_, i64>(0)?,
            path: r.get::<_, String>(1)?,
            person: r.get::<_, Option<i64>>(2)?.map(|v| v != 0),
            animal: r.get::<_, Option<i64>>(3)?.map(|v| v != 0),
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

// ---- read side (IPC) ----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectionRow {
    pub label: String,
    pub category: String,
    pub confidence: f64,
    /// `[x0, y0, x1, y1]` in original-image pixel coords.
    pub bbox: [f64; 4],
}

pub fn detections_for_image(
    conn: &Connection,
    image_id: i64,
) -> Result<Vec<DetectionRow>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT label, category, confidence, bbox_x0, bbox_y0, bbox_x1, bbox_y1
         FROM image_detections WHERE image_id = ?1 ORDER BY confidence DESC",
    )?;
    let rows = stmt.query_map([image_id], |r| {
        Ok(DetectionRow {
            label: r.get(0)?,
            category: r.get(1)?,
            confidence: r.get(2)?,
            bbox: [r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?],
        })
    })?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionRow {
    pub caption: String,
    pub keywords: Vec<String>,
}

pub fn caption_for_image(conn: &Connection, image_id: i64) -> Result<Option<CaptionRow>, LibError> {
    let row = conn
        .query_row(
            "SELECT caption, keywords FROM image_captions WHERE image_id = ?1",
            [image_id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((caption, kw)) = row else {
        return Ok(None);
    };
    // Stored keywords hold only the caption-derived nouns (the captioner runs in the deferred Phase B
    // with no detection context). Union the CURRENT detection labels at read time so a detector re-run
    // is reflected without re-running the expensive captioner. Dedup case-insensitively, stored first.
    let mut keywords: Vec<String> = serde_json::from_str(&kw).unwrap_or_default();
    let mut seen: HashSet<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
    for d in detections_for_image(conn, image_id)? {
        if seen.insert(d.label.to_lowercase()) {
            keywords.push(d.label);
        }
    }
    Ok(Some(CaptionRow { caption, keywords }))
}

/// MobileCLIP presence-probe scores for one image (advisory AI readout; manual labels stay truth).
/// `None` when the probe hasn't run for this image yet.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresenceRow {
    pub p_person: f64,
    pub p_animal: f64,
}

pub fn presence_for_image(
    conn: &Connection,
    image_id: i64,
) -> Result<Option<PresenceRow>, LibError> {
    let row = conn
        .query_row(
            "SELECT p_person, p_animal FROM image_presence WHERE image_id = ?1",
            [image_id],
            |r| {
                Ok(PresenceRow {
                    p_person: r.get(0)?,
                    p_animal: r.get(1)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Manual ground-truth labels (tri-state per field: `None` = unlabeled). Distinct from AI detections.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserLabels {
    pub contains_person: Option<bool>,
    pub contains_animal: Option<bool>,
}

pub fn user_labels(conn: &Connection, image_id: i64) -> Result<UserLabels, LibError> {
    let row = conn
        .query_row(
            "SELECT contains_person, contains_animal FROM image_user_labels WHERE image_id = ?1",
            [image_id],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<i64>>(1)?)),
        )
        .optional()?;
    Ok(row.map_or(UserLabels::default(), |(p, a)| UserLabels {
        contains_person: p.map(|v| v != 0),
        contains_animal: a.map(|v| v != 0),
    }))
}

/// Upsert one label field (`"person"` | `"animal"`) to a tri-state value (`None` clears it).
pub fn set_user_label(
    conn: &Connection,
    image_id: i64,
    field: &str,
    value: Option<bool>,
    now: i64,
) -> Result<(), LibError> {
    // Whitelist → column name (never interpolate caller input directly).
    let col = match field {
        "person" => "contains_person",
        "animal" => "contains_animal",
        _ => return Err(LibError::Other(format!("unknown label field: {field}"))),
    };
    let v: Option<i64> = value.map(|b| b as i64);
    let sql = format!(
        "INSERT INTO image_user_labels(image_id, {col}, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(image_id) DO UPDATE SET {col} = ?2, updated_at = ?3"
    );
    conn.execute(&sql, params![image_id, v, now])?;
    Ok(())
}

/// Upsert one label field across many images in a single transaction (multi-select labeling).
pub fn set_user_label_many(
    conn: &mut Connection,
    image_ids: &[i64],
    field: &str,
    value: Option<bool>,
    now: i64,
) -> Result<(), LibError> {
    let col = match field {
        "person" => "contains_person",
        "animal" => "contains_animal",
        _ => return Err(LibError::Other(format!("unknown label field: {field}"))),
    };
    let v: Option<i64> = value.map(|b| b as i64);
    let sql = format!(
        "INSERT INTO image_user_labels(image_id, {col}, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(image_id) DO UPDATE SET {col} = ?2, updated_at = ?3"
    );
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(&sql)?;
        for &id in image_ids {
            stmt.execute(params![id, v, now])?;
        }
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetRow {
    pub category: String,
    pub count: i64,
}

/// Distinct-image counts per detected category (LeftNav "Detected" facet). No query-side confidence
/// floor: every `image_detections` row was already accepted at write time by its analyzer's per-
/// category bar (D-FINE People 0.55 / Vehicles 0.50; MegaDetector Animals 0.35, both then CLIP-gated).
/// A blanket `>= 0.5` floor here was strictly higher than the Animals bar, so CLIP-confirmed animals
/// scored in [0.35, 0.50) were silently dropped from the facet (and the matching library filter).
pub fn analysis_facets(conn: &Connection) -> Result<Vec<FacetRow>, LibError> {
    // Each category counts present images with a model detection in that bucket OR (for People /
    // Animals) a matching manual ground-truth label OR a presence-probe score over its threshold, so
    // hand-flagged and probe-detected images show up in the nav. All column names are whitelisted
    // constants and the taus are trusted consts — never caller input — so the format! is injection-safe.
    // FacetSpec = (category, manual-label column, (presence-probe column, threshold)).
    type FacetSpec = (
        &'static str,
        Option<&'static str>,
        Option<(&'static str, f64)>,
    );
    let cats: [FacetSpec; 3] = [
        (
            "People",
            Some("contains_person"),
            Some(("p_person", PRESENCE_TAU_PERSON)),
        ),
        (
            "Animals",
            Some("contains_animal"),
            Some(("p_animal", PRESENCE_TAU_ANIMAL)),
        ),
        ("Vehicles", None, None),
    ];
    let mut out = Vec::new();
    for (cat, label_col, probe) in cats {
        let label_clause = match label_col {
            Some(col) => format!(
                " OR EXISTS (SELECT 1 FROM image_user_labels ul \
                   WHERE ul.image_id = i.id AND ul.{col} = 1)"
            ),
            None => String::new(),
        };
        let probe_clause = match probe {
            Some((col, tau)) => format!(
                " OR EXISTS (SELECT 1 FROM image_presence p \
                   WHERE p.image_id = i.id AND p.{col} >= {tau})"
            ),
            None => String::new(),
        };
        let sql = format!(
            "SELECT COUNT(*) FROM images i WHERE i.status = 'present' AND (\
               EXISTS (SELECT 1 FROM image_detections d \
                       WHERE d.image_id = i.id AND d.category = ?1){label_clause}{probe_clause})"
        );
        let count: i64 = conn.query_row(&sql, params![cat], |r| r.get(0))?;
        if count > 0 {
            out.push(FacetRow {
                category: cat.to_string(),
                count,
            });
        }
    }
    Ok(out)
}

/// Count of present images (denominator for analysis progress / status).
pub fn present_image_count(conn: &Connection) -> Result<i64, LibError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM images WHERE status = 'present'",
        [],
        |r| r.get(0),
    )?)
}
