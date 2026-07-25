//! Persistence for AI scan-analysis results (object detection + captioning).
//!
//! Storage is generic over analyzers: the canonical `analysis_results` row (image × analyzer ×
//! model_version) holds the JSON payload; known analyzer ids are also projected into the
//! denormalized `image_detections` / `image_captions` tables for fast filtering and display.
//! Kept free of any ML/ort dependency — it reads the payload JSON directly.

use std::collections::HashSet;

use core_db::rusqlite::{params, Connection, OptionalExtension, ToSql};
use serde::{Deserialize, Serialize};

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

// ---- per-stage attempt record (what ran, when, and whether it failed) ----

/// Pseudo-stage id for panorama detection in the per-image scan record. Panorama keeps its own
/// `pano_detect_scan` marker table (it is version-gated on a different key, `algo_version`), but it
/// also records an attempt here so one photo shows ONE list of stages.
pub const PANORAMA_STAGE_ID: &str = "panorama";

/// A user-selectable scan stage.
///
/// Typed rather than a bare string so an unknown or misspelled id is rejected at the IPC boundary
/// instead of silently selecting nothing — a stage that quietly disappears is indistinguishable
/// from one that ran and found nothing.
///
/// `Objects` and `Animals` are separate detectors with very different costs (MegaDetector is
/// ~1 s/image), which is why they are separately selectable. The CLIP presence probe has no variant:
/// it reuses the verifier those two already load, so it rides along with them rather than being a
/// choice the user has to understand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StageId {
    Objects,
    Animals,
    Faces,
    Captions,
    Panoramas,
}

impl StageId {
    /// Every stage, in the order the UI lists them.
    pub const ALL: [StageId; 5] = [
        StageId::Objects,
        StageId::Animals,
        StageId::Faces,
        StageId::Captions,
        StageId::Panoramas,
    ];

    /// The analyzer id this stage records its results under.
    pub fn analyzer_id(self) -> &'static str {
        match self {
            StageId::Objects => OBJECT_DETECTION_ID,
            StageId::Animals => ANIMAL_DETECTION_ID,
            StageId::Faces => FACE_DETECTION_ID,
            StageId::Captions => CAPTION_ID,
            StageId::Panoramas => PANORAMA_STAGE_ID,
        }
    }
}

/// Analyzer id of the face stage. Defined here (not in the Tauri crate) so the catalog layer can
/// describe the stage without depending on the ML crate; `src-tauri/src/faces.rs` re-exports it.
pub const FACE_DETECTION_ID: &str = "face_detection";

/// One stage's outcome for one image within a single scan. `error: None` means it succeeded.
pub struct StageAttempt<'a> {
    pub stage_id: &'a str,
    pub model_version: &'a str,
    pub error: Option<&'a str>,
}

/// Record the latest attempt for each of `attempts` against `image_id`.
///
/// **Caller owns the transaction, and must use the same one as [`insert_analysis`]** — writing the
/// attempt beside the result is what makes a cancelled scan keep a truthful record of everything it
/// finished, and prevents a crash from leaving a result with no attempt (or vice versa).
///
/// Deliberately separate from `analysis_results`: that table is keyed on
/// `(image_id, analyzer_id, model_version)` and written with `INSERT OR REPLACE`, so recording a
/// failure there would overwrite the previous successful payload on any forced re-scan. Here the key
/// is `(image_id, stage_id)` — one row per stage, latest attempt wins, success or failure.
///
/// Pass only stages that were actually attempted. A stage skipped because it was already clean must
/// NOT be listed: it would overwrite its own newer `ok` row with an older timestamp.
pub fn record_attempts(
    conn: &Connection,
    image_id: i64,
    attempted_at: i64,
    attempts: &[StageAttempt<'_>],
) -> Result<(), LibError> {
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO image_stage_attempt
             (image_id, stage_id, model_version, attempted_at, status, error)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for a in attempts {
        let status = if a.error.is_some() { "error" } else { "ok" };
        stmt.execute(params![
            image_id,
            a.stage_id,
            a.model_version,
            attempted_at,
            status,
            a.error
        ])?;
    }
    Ok(())
}

/// One stage's state for the per-photo readout.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageState {
    pub id: String,
    /// `"ok"` | `"error"` | `"pending"`. `pending` covers both "never attempted" and "last attempted
    /// at an older model version" — i.e. exactly what the next scan would re-run.
    pub status: String,
    /// When the recorded attempt happened; `None` when the stage has never been attempted. Present
    /// even for `pending`, so the UI can say "ran 22 Jul, but with an older model".
    pub attempted_at: Option<i64>,
    pub model_version: Option<String>,
    pub error: Option<String>,
}

/// Per-photo scan record: when it was last visited, and the state of every stage.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageScanState {
    pub last_scan_at: Option<i64>,
    pub stages: Vec<StageState>,
}

/// The scan record for one image, evaluated against the stage versions currently in force.
///
/// `stages` is `(stage_id, current_version)` — the caller (which owns the model constants) decides
/// what "current" means, including the panorama algo version. A stored attempt whose
/// `model_version` differs from the current one reads as `pending`, matching what a scan would
/// actually do rather than what was once true.
pub fn image_scan_state(
    conn: &Connection,
    image_id: i64,
    stages: &[(&str, &str)],
) -> Result<ImageScanState, LibError> {
    let mut stmt = conn.prepare(
        "SELECT model_version, attempted_at, status, error
           FROM image_stage_attempt WHERE image_id = ?1 AND stage_id = ?2",
    )?;
    let mut out = Vec::with_capacity(stages.len());
    let mut last: Option<i64> = None;
    for (id, current_version) in stages {
        let row = stmt
            .query_row(params![image_id, id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .optional()?;
        let state = match row {
            None => StageState {
                id: (*id).to_string(),
                status: "pending".into(),
                attempted_at: None,
                model_version: None,
                error: None,
            },
            Some((version, at, status, error)) => {
                last = Some(last.map_or(at, |l: i64| l.max(at)));
                let stale_version = version != *current_version;
                StageState {
                    id: (*id).to_string(),
                    status: if stale_version {
                        "pending".into()
                    } else {
                        status
                    },
                    attempted_at: Some(at),
                    model_version: Some(version),
                    error,
                }
            }
        };
        out.push(state);
    }
    Ok(ImageScanState {
        last_scan_at: last,
        stages: out,
    })
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
        out.push(AnalyzeTarget {
            id,
            path,
            content_hash_hex: hash_hex(&hb),
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
    let sql = stale_sql(
        stages.len(),
        true,
        " AND i.id > ?",
        " ORDER BY i.id LIMIT ?",
    );
    // Params appear in SQL order: per-stage (analyzer_id, model_version) inside the JOINs, then the
    // cursor, then the limit.
    let mut binds: Vec<&dyn ToSql> = stage_binds(stages);
    binds.push(&cursor);
    binds.push(&limit);
    map_stale_rows(conn, &sql, &binds, stages.len())
}

/// COUNT of present images with ≥1 stale stage — the denominator for scan progress. Same join shape
/// as [`stale_targets`], without pagination.
pub fn stale_count(conn: &Connection, stages: &[StageSpec]) -> Result<i64, LibError> {
    if stages.is_empty() {
        return Ok(0);
    }
    let sql = stale_sql(stages.len(), false, "", "");
    let binds = stage_binds(stages);
    Ok(conn.query_row(&sql, binds.as_slice(), |r| r.get(0))?)
}

// ---- shared SQL construction for the stale-stage pagers ----

/// Builds the dirty-stage query shared by [`stale_targets`], [`stale_count`] and their scoped
/// `*_in` counterparts: one `LEFT JOIN analysis_results j{k}` per stage keyed on
/// `(analyzer_id, model_version, status='ok')`, and a `j{k}.image_id IS NULL` disjunction.
///
/// `with_flags` selects the target shape (`id, path, content_hash` + one staleness flag per stage)
/// versus `COUNT(*)`. `extra_where` is spliced after the fixed `i.status = 'present'` predicate and
/// `tail` after the disjunction; both MUST contain only `?` placeholders, whose binds the caller
/// appends **after** the per-stage pairs. Aliases and column counts derive from `n` (internal,
/// trusted); every analyzer id / model version is a bound parameter, so this stays injection-safe.
fn stale_sql(n: usize, with_flags: bool, extra_where: &str, tail: &str) -> String {
    let mut sql = if with_flags {
        let mut s = String::from("SELECT i.id, i.path, i.content_hash");
        for k in 0..n {
            s.push_str(&format!(", (j{k}.image_id IS NULL)"));
        }
        s
    } else {
        String::from("SELECT COUNT(*)")
    };
    sql.push_str(" FROM images i");
    for k in 0..n {
        sql.push_str(&format!(
            " LEFT JOIN analysis_results j{k} ON j{k}.image_id = i.id \
             AND j{k}.analyzer_id = ? AND j{k}.model_version = ? AND j{k}.status = 'ok'"
        ));
    }
    sql.push_str(" WHERE i.status = 'present'");
    sql.push_str(extra_where);
    sql.push_str(" AND (");
    for k in 0..n {
        if k > 0 {
            sql.push_str(" OR ");
        }
        sql.push_str(&format!("j{k}.image_id IS NULL"));
    }
    sql.push(')');
    sql.push_str(tail);
    sql
}

/// The leading `(analyzer_id, model_version)` bind pairs consumed by the per-stage JOINs.
fn stage_binds(stages: &[StageSpec]) -> Vec<&dyn ToSql> {
    let mut binds: Vec<&dyn ToSql> = Vec::with_capacity(stages.len() * 2);
    for s in stages {
        binds.push(&s.analyzer_id);
        binds.push(&s.model_version);
    }
    binds
}

/// Runs a `with_flags` [`stale_sql`] query and decodes it into [`StaleTarget`]s.
fn map_stale_rows(
    conn: &Connection,
    sql: &str,
    binds: &[&dyn ToSql],
    n: usize,
) -> Result<Vec<StaleTarget>, LibError> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(binds, |r| {
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
        out.push(StaleTarget {
            id,
            path,
            content_hash_hex: hash_hex(&hb),
            stale,
        });
    }
    Ok(out)
}

/// A 32-byte BLAKE3 content hash as lowercase hex; empty string for a malformed/legacy blob.
fn hash_hex(hb: &[u8]) -> String {
    if hb.len() == 32 {
        let mut a = [0u8; 32];
        a.copy_from_slice(hb);
        core_raw::hex(&a)
    } else {
        String::new()
    }
}

/// `?,?,…` for an `IN` list of `n` bound ids.
fn placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for k in 0..n {
        if k > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s
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
        out.push(AnalyzeTarget {
            id,
            path,
            content_hash_hex: hash_hex(&hb),
        });
    }
    Ok(out)
}

// ---- scan scoping (run a pass over one folder / date / collection instead of the library) ----

/// Largest `IN (…)` list the scoped selectors bind in one statement. Callers page their id list
/// through `chunks(SCOPE_CHUNK)`; well under SQLite's variable limit with room for the per-stage
/// binds that precede it.
pub const SCOPE_CHUNK: usize = 400;

/// The container-only subset of [`crate::query::QueryParams`] that may narrow an AI scan.
///
/// Deliberately **not** `QueryParams`: serde ignores unknown fields, so a frontend that posts its
/// whole library filter has the non-container dimensions (`minStars`, `flag`, `colorLabel`,
/// `search`, `detectedCategory`, `personId`) dropped *by this type* rather than by caller
/// discipline. The last two are produced **by** the scans themselves, so honouring them would make
/// a scan's input depend on its own previous output; the rest are triage state, not containers.
///
/// Same defence-in-depth shape as [`crate::query::rejected_ids`] forcing `flag = 'reject'`: the
/// narrow type is the guarantee.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanScope {
    pub folder_id: Option<i64>,
    pub capture_year: Option<String>,
    pub capture_date: Option<String>,
    pub collection_id: Option<i64>,
    pub keyword_id: Option<i64>,
    pub import_session_id: Option<i64>,
    pub format: Option<String>,
}

impl ScanScope {
    /// True when nothing narrows the scan — the caller keeps the library-wide keyset path.
    pub fn is_empty(&self) -> bool {
        self.folder_id.is_none()
            && self.capture_year.is_none()
            && self.capture_date.is_none()
            && self.collection_id.is_none()
            && self.keyword_id.is_none()
            && self.import_session_id.is_none()
            && self.format.is_none()
    }
}

/// Pending work for one stage, sizing its row in the scan modal.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StagePending {
    pub stage: StageId,
    /// Images this stage would visit. `None` when the stage's models aren't installed, so the UI
    /// shows "not installed" rather than a misleading 0.
    pub pending: Option<i64>,
    pub models_ready: bool,
    /// True when the figure ignores `scope` — panorama detection has no scoped entry point, so its
    /// count is always library-wide and the modal says so.
    pub library_wide: bool,
}

/// Scoped work estimate for the scan modal: how many images the current view holds, and how much
/// work each stage still has there.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeCounts {
    pub total: i64,
    pub stages: Vec<StagePending>,
}

/// Ascending ids of the present images inside `scope`.
///
/// Predicates mirror the matching clauses of `query::WHERE` exactly (same `strftime` formatting,
/// same `EXISTS` membership subqueries) so the scanned set is the set the grid is showing. One
/// deliberate divergence: camera companions are **not** excluded — the library-wide pass analyzes
/// every `status='present'` row including paired JPEG/HEIFs, and a scoped pass must not silently
/// analyze less per image than an unscoped one.
///
/// An empty scope returns every present id; callers should branch on [`ScanScope::is_empty`] first
/// and keep the cheaper keyset walk instead.
pub fn scope_image_ids(conn: &Connection, scope: &ScanScope) -> Result<Vec<i64>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT i.id FROM images i
          WHERE i.status = 'present'
            AND (:folder_id IS NULL OR i.folder_id = :folder_id)
            AND (:capture_year IS NULL
                 OR strftime('%Y', i.capture_date, 'unixepoch') = :capture_year)
            AND (:capture_date IS NULL
                 OR strftime('%Y-%m-%d', i.capture_date, 'unixepoch') = :capture_date)
            AND (:collection_id IS NULL OR EXISTS
                 (SELECT 1 FROM collection_images ci
                   WHERE ci.image_id = i.id AND ci.collection_id = :collection_id))
            AND (:keyword_id IS NULL OR EXISTS
                 (SELECT 1 FROM image_keywords ik
                   WHERE ik.image_id = i.id AND ik.keyword_id = :keyword_id))
            AND (:import_session_id IS NULL OR i.import_session_id = :import_session_id)
            AND (:format IS NULL OR i.format = :format)
          ORDER BY i.id",
    )?;
    let rows = stmt.query_map(
        core_db::rusqlite::named_params! {
            ":folder_id": scope.folder_id,
            ":capture_year": scope.capture_year,
            ":capture_date": scope.capture_date,
            ":collection_id": scope.collection_id,
            ":keyword_id": scope.keyword_id,
            ":import_session_id": scope.import_session_id,
            ":format": scope.format,
        },
        |r| r.get::<_, i64>(0),
    )?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// [`stale_targets`] restricted to `ids` instead of a keyset cursor — the scoped scan's pager.
/// `ids` is one page (≤ [`SCOPE_CHUNK`]); the caller walks its scope id list in chunks.
pub fn stale_targets_in(
    conn: &Connection,
    stages: &[StageSpec],
    ids: &[i64],
) -> Result<Vec<StaleTarget>, LibError> {
    if stages.is_empty() || ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = stale_sql(
        stages.len(),
        true,
        &format!(" AND i.id IN ({})", placeholders(ids.len())),
        " ORDER BY i.id",
    );
    let mut binds: Vec<&dyn ToSql> = stage_binds(stages);
    binds.extend(ids.iter().map(|id| id as &dyn ToSql));
    map_stale_rows(conn, &sql, &binds, stages.len())
}

/// COUNT of images in `ids` with ≥1 stale stage — the scoped scan's progress denominator. Chunks
/// `ids` internally, so the full scope id list can be passed in one call.
pub fn stale_count_in(
    conn: &Connection,
    stages: &[StageSpec],
    ids: &[i64],
) -> Result<i64, LibError> {
    if stages.is_empty() {
        return Ok(0);
    }
    let mut total = 0i64;
    for chunk in ids.chunks(SCOPE_CHUNK) {
        let sql = stale_sql(
            stages.len(),
            false,
            &format!(" AND i.id IN ({})", placeholders(chunk.len())),
            "",
        );
        let mut binds: Vec<&dyn ToSql> = stage_binds(stages);
        binds.extend(chunk.iter().map(|id| id as &dyn ToSql));
        total += conn.query_row(&sql, binds.as_slice(), |r| r.get::<_, i64>(0))?;
    }
    Ok(total)
}

/// [`present_targets_after`] restricted to `ids` — the scoped FORCED re-scan pager (ignores
/// staleness; the caller treats every stage as stale). `ids` is one page (≤ [`SCOPE_CHUNK`]).
pub fn present_targets_in(conn: &Connection, ids: &[i64]) -> Result<Vec<AnalyzeTarget>, LibError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT id, path, content_hash FROM images
          WHERE status = 'present' AND id IN ({}) ORDER BY id",
        placeholders(ids.len())
    );
    let binds: Vec<&dyn ToSql> = ids.iter().map(|id| id as &dyn ToSql).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(binds.as_slice(), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, path, hb) = row?;
        out.push(AnalyzeTarget {
            id,
            path,
            content_hash_hex: hash_hex(&hb),
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

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::rusqlite::params;
    use core_db::Db;

    const STAGE_A: StageSpec = StageSpec {
        analyzer_id: OBJECT_DETECTION_ID,
        model_version: "v1",
    };
    const STAGE_B: StageSpec = StageSpec {
        analyzer_id: CAPTION_ID,
        model_version: "v1",
    };

    /// A present image; `capture` is epoch seconds (NULL when `None`). Returns its id.
    fn img(conn: &Connection, tag: i64, capture: Option<i64>) -> i64 {
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                 capture_date, format, imported_at)
             VALUES (?1, 1, ?2, ?3, 'present', ?4, 'raw', 0)",
            params![
                vec![tag as u8; 32],
                format!("/lib/{tag}.cr3"),
                format!("{tag}.cr3"),
                capture,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Mark `stage` done at `version` for `id` (what `insert_analysis` writes on success).
    fn mark(conn: &Connection, id: i64, stage: &StageSpec, version: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO analysis_results
                 (image_id, analyzer_id, model_version, ran_at, status, payload)
             VALUES (?1, ?2, ?3, 0, 'ok', '{}')",
            params![id, stage.analyzer_id, version],
        )
        .unwrap();
    }

    fn ids(conn: &Connection, scope: &ScanScope) -> Vec<i64> {
        scope_image_ids(conn, scope).unwrap()
    }

    // ---- scope resolution, one dimension at a time ----

    #[test]
    fn empty_scope_is_empty_and_selects_everything() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        let b = img(&db.conn, 2, Some(0));
        let scope = ScanScope::default();
        assert!(scope.is_empty());
        assert_eq!(ids(&db.conn, &scope), vec![a, b]);
    }

    #[test]
    fn any_single_dimension_makes_the_scope_non_empty() {
        assert!(!ScanScope {
            folder_id: Some(1),
            ..Default::default()
        }
        .is_empty());
        assert!(!ScanScope {
            format: Some("raw".into()),
            ..Default::default()
        }
        .is_empty());
        assert!(!ScanScope {
            capture_date: Some("2026-06-22".into()),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn scope_by_folder() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO folders(id, path, added_at) VALUES (7, '/lib/a', 0), (8, '/lib/b', 0)",
                [],
            )
            .unwrap();
        let a = img(&db.conn, 1, Some(0));
        let b = img(&db.conn, 2, Some(0));
        db.conn
            .execute("UPDATE images SET folder_id = 7 WHERE id = ?1", params![a])
            .unwrap();
        db.conn
            .execute("UPDATE images SET folder_id = 8 WHERE id = ?1", params![b])
            .unwrap();
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    folder_id: Some(7),
                    ..Default::default()
                }
            ),
            vec![a]
        );
    }

    #[test]
    fn scope_by_capture_year_and_day() {
        let db = Db::open_in_memory().unwrap();
        // 2026-06-22T00:00:00Z and 2026-06-23T00:00:00Z.
        let d22 = img(&db.conn, 1, Some(1_782_086_400));
        let d23 = img(&db.conn, 2, Some(1_782_172_800));
        // 2025-01-01T00:00:00Z.
        let y25 = img(&db.conn, 3, Some(1_735_689_600));
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    capture_date: Some("2026-06-22".into()),
                    ..Default::default()
                }
            ),
            vec![d22]
        );
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    capture_year: Some("2026".into()),
                    ..Default::default()
                }
            ),
            vec![d22, d23]
        );
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    capture_year: Some("2025".into()),
                    ..Default::default()
                }
            ),
            vec![y25]
        );
    }

    #[test]
    fn scope_by_collection_and_keyword() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        let b = img(&db.conn, 2, Some(0));
        db.conn
            .execute("INSERT INTO collections(id, name) VALUES (3, 'trip')", [])
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO collection_images(collection_id, image_id) VALUES (3, ?1)",
                params![b],
            )
            .unwrap();
        db.conn
            .execute("INSERT INTO keywords(id, name) VALUES (5, 'denmark')", [])
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO image_keywords(image_id, keyword_id) VALUES (?1, 5)",
                params![a],
            )
            .unwrap();
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    collection_id: Some(3),
                    ..Default::default()
                }
            ),
            vec![b]
        );
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    keyword_id: Some(5),
                    ..Default::default()
                }
            ),
            vec![a]
        );
    }

    #[test]
    fn scope_by_import_session_and_format() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO import_sessions(id, mode, started_at) VALUES (9, 'copy', 0)",
                [],
            )
            .unwrap();
        let a = img(&db.conn, 1, Some(0));
        let b = img(&db.conn, 2, Some(0));
        db.conn
            .execute(
                "UPDATE images SET import_session_id = 9 WHERE id = ?1",
                params![a],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE images SET format = 'jpeg' WHERE id = ?1",
                params![b],
            )
            .unwrap();
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    import_session_id: Some(9),
                    ..Default::default()
                }
            ),
            vec![a]
        );
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    format: Some("jpeg".into()),
                    ..Default::default()
                }
            ),
            vec![b]
        );
    }

    #[test]
    fn dimensions_intersect_and_missing_images_are_excluded() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(1_782_086_400)); // 2026-06-22, raw
        let b = img(&db.conn, 2, Some(1_782_086_400)); // 2026-06-22, jpeg
        let c = img(&db.conn, 3, Some(1_782_086_400)); // 2026-06-22, raw but missing
        db.conn
            .execute(
                "UPDATE images SET format = 'jpeg' WHERE id = ?1",
                params![b],
            )
            .unwrap();
        db.conn
            .execute(
                "UPDATE images SET status = 'missing' WHERE id = ?1",
                params![c],
            )
            .unwrap();
        assert_eq!(
            ids(
                &db.conn,
                &ScanScope {
                    capture_date: Some("2026-06-22".into()),
                    format: Some("raw".into()),
                    ..Default::default()
                }
            ),
            vec![a],
            "intersection of date+format, and 'missing' never scanned"
        );
    }

    // ---- scoped staleness ----

    #[test]
    fn scoped_count_matches_unscoped_over_the_whole_library() {
        let db = Db::open_in_memory().unwrap();
        let all: Vec<i64> = (1..=6).map(|t| img(&db.conn, t, Some(0))).collect();
        mark(&db.conn, all[0], &STAGE_A, "v1");
        mark(&db.conn, all[1], &STAGE_A, "v1");
        let specs = [STAGE_A];
        assert_eq!(
            stale_count_in(&db.conn, &specs, &all).unwrap(),
            stale_count(&db.conn, &specs).unwrap()
        );
        assert_eq!(stale_count(&db.conn, &specs).unwrap(), 4);
    }

    #[test]
    fn scoped_targets_respect_per_stage_dirty_flags() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        let b = img(&db.conn, 2, Some(0));
        // `a` is done for both stages; `b` only for the caption stage.
        mark(&db.conn, a, &STAGE_A, "v1");
        mark(&db.conn, a, &STAGE_B, "v1");
        mark(&db.conn, b, &STAGE_B, "v1");
        let specs = [STAGE_A, STAGE_B];

        let targets = stale_targets_in(&db.conn, &specs, &[a, b]).unwrap();
        assert_eq!(targets.len(), 1, "only `b` has work");
        assert_eq!(targets[0].id, b);
        assert_eq!(
            targets[0].stale,
            vec![true, false],
            "detection stale, caption already done"
        );

        // Bumping ONE stage's version re-dirties only that stage.
        let bumped = [
            StageSpec {
                analyzer_id: OBJECT_DETECTION_ID,
                model_version: "v1",
            },
            StageSpec {
                analyzer_id: CAPTION_ID,
                model_version: "v2",
            },
        ];
        let targets = stale_targets_in(&db.conn, &bumped, &[a]).unwrap();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].stale, vec![false, true]);
    }

    #[test]
    fn errored_stage_rows_count_as_stale() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        db.conn
            .execute(
                "INSERT INTO analysis_results
                     (image_id, analyzer_id, model_version, ran_at, status, payload)
                 VALUES (?1, ?2, 'v1', 0, 'error', '{}')",
                params![a, OBJECT_DETECTION_ID],
            )
            .unwrap();
        assert_eq!(stale_count_in(&db.conn, &[STAGE_A], &[a]).unwrap(), 1);
    }

    #[test]
    fn scoped_selectors_span_the_chunk_boundary() {
        let db = Db::open_in_memory().unwrap();
        let n = SCOPE_CHUNK + 37;
        let all: Vec<i64> = (0..n)
            .map(|t| img(&db.conn, t as i64 % 251, Some(0)))
            .collect();
        // stale_count_in chunks internally: every id must be counted exactly once.
        assert_eq!(
            stale_count_in(&db.conn, &[STAGE_A], &all).unwrap(),
            n as i64
        );

        // The paged selectors take one chunk at a time; the union must be the whole set with no
        // duplicates and ascending ids within a page.
        let mut seen: Vec<i64> = Vec::new();
        for chunk in all.chunks(SCOPE_CHUNK) {
            let page = stale_targets_in(&db.conn, &[STAGE_A], chunk).unwrap();
            assert!(page.windows(2).all(|w| w[0].id < w[1].id));
            seen.extend(page.into_iter().map(|t| t.id));
        }
        assert_eq!(seen, all);
        assert_eq!(
            present_targets_in(&db.conn, &all[..SCOPE_CHUNK])
                .unwrap()
                .len(),
            SCOPE_CHUNK
        );
    }

    #[test]
    fn empty_inputs_are_no_ops() {
        let db = Db::open_in_memory().unwrap();
        img(&db.conn, 1, Some(0));
        assert!(stale_targets_in(&db.conn, &[STAGE_A], &[])
            .unwrap()
            .is_empty());
        assert!(present_targets_in(&db.conn, &[]).unwrap().is_empty());
        assert_eq!(stale_count_in(&db.conn, &[STAGE_A], &[]).unwrap(), 0);
        assert_eq!(stale_count_in(&db.conn, &[], &[1]).unwrap(), 0);
    }

    // ---- attempt record: what ran, when, and whether it failed ----

    fn attempt(
        stage: &str,
        version: &str,
        error: Option<&str>,
    ) -> (String, String, Option<String>) {
        (
            stage.to_string(),
            version.to_string(),
            error.map(str::to_string),
        )
    }

    /// `record_attempts` over owned tuples, mirroring how the scan builds them across the rayon
    /// boundary.
    fn record(conn: &Connection, id: i64, at: i64, rows: &[(String, String, Option<String>)]) {
        let borrowed: Vec<StageAttempt<'_>> = rows
            .iter()
            .map(|(s, v, e)| StageAttempt {
                stage_id: s,
                model_version: v,
                error: e.as_deref(),
            })
            .collect();
        record_attempts(conn, id, at, &borrowed).unwrap();
    }

    fn specs_now() -> Vec<(&'static str, &'static str)> {
        vec![(OBJECT_DETECTION_ID, "v1"), (CAPTION_ID, "v1")]
    }

    #[test]
    fn attempts_record_success_and_failure_without_touching_results() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        // A real success also writes the canonical result row.
        mark(&db.conn, a, &STAGE_A, "v1");
        record(
            &db.conn,
            a,
            500,
            &[
                attempt(OBJECT_DETECTION_ID, "v1", None),
                attempt(CAPTION_ID, "v1", Some("onnx blew up")),
            ],
        );

        let state = image_scan_state(&db.conn, a, &specs_now()).unwrap();
        assert_eq!(state.last_scan_at, Some(500));
        assert_eq!(state.stages[0].status, "ok");
        assert_eq!(state.stages[1].status, "error");
        assert_eq!(state.stages[1].error.as_deref(), Some("onnx blew up"));

        // The failure must NOT have manufactured a canonical result row.
        let results: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM analysis_results WHERE image_id = ?1",
                params![a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(results, 1, "only the successful stage has a result row");
    }

    #[test]
    fn a_failed_attempt_never_destroys_the_previous_success() {
        // The reason failures live in their own table: `analysis_results` is INSERT OR REPLACE on
        // (image, analyzer, version), so a forced re-scan that fails would otherwise overwrite the
        // last good payload.
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        mark(&db.conn, a, &STAGE_A, "v1");
        record(
            &db.conn,
            a,
            100,
            &[attempt(OBJECT_DETECTION_ID, "v1", None)],
        );

        record(
            &db.conn,
            a,
            900,
            &[attempt(OBJECT_DETECTION_ID, "v1", Some("gpu fell over"))],
        );

        let still_ok: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM analysis_results
                  WHERE image_id = ?1 AND analyzer_id = ?2 AND status = 'ok'",
                params![a, OBJECT_DETECTION_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still_ok, 1, "the previous successful result must survive");

        let state = image_scan_state(&db.conn, a, &specs_now()).unwrap();
        assert_eq!(
            state.stages[0].status, "error",
            "latest attempt is the error"
        );
        assert_eq!(state.stages[0].attempted_at, Some(900));
    }

    #[test]
    fn attempts_are_idempotent_and_retryable() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        let fail = [attempt(OBJECT_DETECTION_ID, "v1", Some("boom"))];
        record(&db.conn, a, 100, &fail);
        record(&db.conn, a, 200, &fail);
        let rows: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM image_stage_attempt WHERE image_id = ?1",
                params![a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            rows, 1,
            "one row per (image, stage) — repeats update in place"
        );

        // An errored stage stays stale, so the scan retries it...
        assert_eq!(stale_count_in(&db.conn, &[STAGE_A], &[a]).unwrap(), 1);
        // ...and a later success overwrites the failure.
        record(
            &db.conn,
            a,
            300,
            &[attempt(OBJECT_DETECTION_ID, "v1", None)],
        );
        let state = image_scan_state(&db.conn, a, &specs_now()).unwrap();
        assert_eq!(state.stages[0].status, "ok");
        assert_eq!(state.stages[0].error, None);
    }

    #[test]
    fn an_attempt_at_an_older_model_version_reads_as_pending() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        record(
            &db.conn,
            a,
            100,
            &[attempt(OBJECT_DETECTION_ID, "v1", None)],
        );

        // Current version moved on — the readout must agree with what the scan would do.
        let bumped = vec![(OBJECT_DETECTION_ID, "v2"), (CAPTION_ID, "v1")];
        let state = image_scan_state(&db.conn, a, &bumped).unwrap();
        assert_eq!(state.stages[0].status, "pending");
        assert_eq!(
            state.stages[0].attempted_at,
            Some(100),
            "the old attempt is still reported so the UI can explain WHY it is pending"
        );
        assert_eq!(state.stages[0].model_version.as_deref(), Some("v1"));
    }

    #[test]
    fn a_never_attempted_stage_is_pending_with_no_timestamp() {
        let db = Db::open_in_memory().unwrap();
        let a = img(&db.conn, 1, Some(0));
        let state = image_scan_state(&db.conn, a, &specs_now()).unwrap();
        assert_eq!(state.last_scan_at, None);
        assert!(state.stages.iter().all(|s| s.status == "pending"));
        assert!(state.stages.iter().all(|s| s.attempted_at.is_none()));
    }
}
