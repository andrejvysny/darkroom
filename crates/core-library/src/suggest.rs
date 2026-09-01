//! Pick/reject suggestions: catalog → training samples, training + promotion, and the scoring pass.
//!
//! `core-suggest` is pure math and knows nothing about SQLite; this module is the whole seam between
//! it and the catalog. Three jobs:
//!
//! - **Assembly** — turn `ratings_flags` + `user_events` + `image_features`/`face`/EXIF + the stored
//!   CLIP embedding into [`core_suggest::Sample`]s. The label's *provenance* (was a suggestion on
//!   screen when the user acted?) comes from the event log, because that is the only place that
//!   records what the user was looking at.
//! - **Training + promotion** — every fit is appended to `suggestion_model`; which one is LIVE is a
//!   single pointer in `app_meta`, and a retrain only takes it over if it does not lose materially
//!   against the incumbent's out-of-fold AUPRC.
//! - **Scoring** — one row per embedded image in `image_suggestion`, including a small deterministic
//!   withheld slice whose badge the UI hides so those labels stay uninfluenced.
//!
//! The hand features are built by ONE function for both training and scoring over the SAME universe,
//! so the burst-relative ranks a model was fit against are the ranks it later scores against.

use std::collections::{BTreeMap, HashMap};

use core_db::rusqlite::{named_params, params, Connection, OptionalExtension, Row};
use core_suggest::{assemble, HandFeatures, LabelProvenance, Model, Sample};
use serde::Serialize;

use crate::error::LibError;
use crate::settings::{get_meta, set_meta};

/// `app_meta` key pointing at the LIVE model row (`suggestion_model.id`, decimal text).
const KEY_CURRENT_MODEL: &str = "suggest_current_model_id";

/// Folds for the group-aware λ sweep.
const CV_FOLDS: usize = 5;

/// How much out-of-fold AUPRC a retrain may LOSE and still be promoted. Demanding an improvement
/// would freeze the model the first time fold noise moved the number down; promoting unconditionally
/// would let a genuinely worse fit take over the badges.
const PROMOTE_AUPRC_SLACK: f32 = 0.02;

/// Percent of scored images whose badge is withheld. Those images keep collecting *unprompted*
/// labels, the only kind that can honestly measure the model afterwards.
const WITHHELD_PERCENT: u64 = 8;

/// Frame-number gap within one filename prefix that still counts as the same burst (ported from the
/// presence probe's `train_presence` grouping, so both models share a notion of "burst").
const GROUP_GAP: i64 = 10;

/// Rows written per transaction in the scoring pass.
const SCORE_BATCH: usize = 500;

// ── feature assembly ─────────────────────────────────────────────────────────

/// One image's model inputs minus the embedding, which is streamed separately — 512 f32 per image is
/// ~100 MB over a 50k library and the scoring pass never needs two of them at once.
#[derive(Debug, Clone)]
struct HandRow {
    id: i64,
    group: u64,
    hand: HandFeatures,
}

/// Per-image face summary over faces the user has not dismissed.
struct FaceAgg {
    count: f32,
    max_det: f32,
    max_quality: f32,
}

fn face_aggregates(conn: &Connection) -> Result<HashMap<i64, FaceAgg>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT asset_id, COUNT(*), MAX(det_score), MAX(quality_score)
           FROM face WHERE status NOT IN ('rejected','ignored') GROUP BY asset_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            FaceAgg {
                count: r.get::<_, i64>(1)? as f32,
                max_det: r.get::<_, f64>(2)? as f32,
                max_quality: r.get::<_, f64>(3)? as f32,
            },
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, agg) = row?;
        out.insert(id, agg);
    }
    Ok(out)
}

/// `"1/250"` | `"2.5s"` | `"0.004"` → seconds. `core-raw::meta` writes the first two forms; anything
/// else — including the `"0"` it emits for a degenerate rational — is *unknown*, not zero.
fn parse_shutter_seconds(s: &str) -> Option<f32> {
    let v = if let Some(d) = s.strip_prefix("1/") {
        d.trim().parse::<f32>().ok().map(|d| 1.0 / d)
    } else {
        s.trim().trim_end_matches('s').trim().parse::<f32>().ok()
    }?;
    (v.is_finite() && v > 0.0).then_some(v)
}

/// Build the hand features for one row of [`hand_query`]'s column layout.
fn hand_from_row(r: &Row<'_>, face: Option<&FaceAgg>) -> core_db::rusqlite::Result<HandFeatures> {
    // Missing is NaN, never 0.0 — a zero-fill would teach the model that "unknown" looks like
    // "dark / unsharp / wide open".
    let opt = |i: usize| -> core_db::rusqlite::Result<f32> {
        Ok(r.get::<_, Option<f64>>(i)?
            .map(|v| v as f32)
            .unwrap_or(f32::NAN))
    };
    let sharpness = opt(3)?;
    let iso: Option<i64> = r.get(8)?;
    let shutter: Option<String> = r.get(9)?;
    Ok(HandFeatures {
        // Laplacian variance spans orders of magnitude; log-compress it like the presence probe.
        sharpness_log: (1.0 + sharpness).ln(),
        clip_hi: opt(4)?,
        clip_lo: opt(5)?,
        dynamic_range_ev: opt(6)?,
        mean_log_luma: opt(7)?,
        // "No face row" is a known zero count but unknown maxima, not a zero-quality face.
        face_count: face.map_or(0.0, |f| f.count),
        face_max_det: face.map_or(f32::NAN, |f| f.max_det),
        face_max_quality: face.map_or(f32::NAN, |f| f.max_quality),
        has_face: if face.is_some() { 1.0 } else { 0.0 },
        log_iso: iso.filter(|&v| v > 0).map_or(f32::NAN, |v| (v as f32).ln()),
        log_shutter: shutter
            .as_deref()
            .and_then(parse_shutter_seconds)
            .map_or(f32::NAN, f32::ln),
        aperture: opt(10)?,
        focal: opt(11)?,
        // Ranks are relative, so "no burst to compare against" is a neutral 0.5, not unknown. Filled
        // in by `fill_ranks` once the grouping is known.
        rank_sharpness: 0.5,
        rank_face_quality: 0.5,
        rank_iso: 0.5,
    })
}

/// Present images carrying an embedding from `embedding_tag`, with everything the hand features are
/// derived from. Column order is the contract [`hand_from_row`] reads.
const HAND_QUERY: &str = "SELECT i.id, i.original_filename, i.capture_fingerprint,
            f.sharpness, f.clip_hi, f.clip_lo, f.dynamic_range_ev, f.mean_log_luma,
            i.iso, i.shutter, i.aperture, i.focal_length
       FROM images i
       JOIN image_embedding e ON e.image_id = i.id
       LEFT JOIN image_features f ON f.image_id = i.id
      WHERE i.status = 'present' AND e.model_tag = :tag
      ORDER BY i.id";

/// The scorable universe: one [`HandRow`] per present image with a matching-tag embedding, grouped
/// into bursts and with the burst-relative ranks filled in.
fn load_hand_rows(conn: &Connection, embedding_tag: &str) -> Result<Vec<HandRow>, LibError> {
    let faces = face_aggregates(conn)?;
    let mut stmt = conn.prepare(HAND_QUERY)?;
    let rows = stmt.query_map(named_params! { ":tag": embedding_tag }, |r| {
        let id: i64 = r.get(0)?;
        Ok((
            id,
            r.get::<_, String>(1)?,
            r.get::<_, Option<Vec<u8>>>(2)?,
            hand_from_row(r, faces.get(&id))?,
        ))
    })?;
    let mut keys: Vec<(i64, String, Option<Vec<u8>>)> = Vec::new();
    let mut hands: Vec<(i64, HandFeatures)> = Vec::new();
    for row in rows {
        let (id, filename, fingerprint, hand) = row?;
        keys.push((id, filename, fingerprint));
        hands.push((id, hand));
    }
    let groups = compute_groups(&keys);
    let mut out: Vec<HandRow> = hands
        .into_iter()
        .map(|(id, hand)| HandRow {
            id,
            group: groups.get(&id).copied().unwrap_or(0),
            hand,
        })
        .collect();
    fill_ranks(&mut out);
    Ok(out)
}

/// `("IMG_", 1234)` from `IMG_1234.CR3` — the trailing digit run is the frame number.
fn frame_key(filename: &str) -> Option<(String, i64)> {
    let stem = std::path::Path::new(filename).file_stem()?.to_str()?;
    let split = stem
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|i| i + 1)
        .unwrap_or(0);
    let (pre, num) = stem.split_at(split);
    Some((pre.to_string(), num.parse().ok()?))
}

/// Burst id per image.
///
/// `capture_fingerprint` is authoritative where present (same shutter actuation → same group, which
/// also keeps a RAW and its camera JPEG together). Everything else falls back to filename prefix +
/// frame proximity, and an unparseable name gets a group of its own: a singleton sharing a group id
/// would invent preference pairs and leak across the CV split.
fn compute_groups(keys: &[(i64, String, Option<Vec<u8>>)]) -> HashMap<i64, u64> {
    let mut out = HashMap::new();
    let mut next = 0u64;
    let mut by_fingerprint: BTreeMap<Vec<u8>, u64> = BTreeMap::new();
    let mut by_name: Vec<(String, i64, i64)> = Vec::new();
    for (id, filename, fingerprint) in keys {
        match fingerprint {
            Some(fp) if !fp.is_empty() => {
                let g = *by_fingerprint.entry(fp.clone()).or_insert_with(|| {
                    next += 1;
                    next - 1
                });
                out.insert(*id, g);
            }
            _ => match frame_key(filename) {
                Some((prefix, frame)) => by_name.push((prefix, frame, *id)),
                None => {
                    out.insert(*id, next);
                    next += 1;
                }
            },
        }
    }

    by_name.sort();
    let mut current = next;
    let mut prev: Option<(String, i64)> = None;
    for (prefix, frame, id) in by_name {
        if prev
            .as_ref()
            .is_none_or(|(p, n)| *p != prefix || frame - n > GROUP_GAP)
        {
            current = next;
            next += 1;
        }
        out.insert(id, current);
        prev = Some((prefix, frame));
    }
    out
}

/// Fill the three burst-relative ranks in place. Groups of one keep the neutral 0.5.
fn fill_ranks(rows: &mut [HandRow]) {
    let mut by_group: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_group.entry(r.group).or_default().push(i);
    }
    for idx in by_group.values().filter(|v| v.len() >= 2) {
        rank_into(
            rows,
            idx,
            |h| h.sharpness_log,
            false,
            |h, v| h.rank_sharpness = v,
        );
        rank_into(
            rows,
            idx,
            |h| h.face_max_quality,
            false,
            |h, v| h.rank_face_quality = v,
        );
        // Lower ISO is the better frame, so this rank is inverted.
        rank_into(rows, idx, |h| h.log_iso, true, |h, v| h.rank_iso = v);
    }
}

/// Rank each finite value among its group's finite values, into `[0, 1]` (1 = best).
fn rank_into(
    rows: &mut [HandRow],
    idx: &[usize],
    get: fn(&HandFeatures) -> f32,
    invert: bool,
    set: fn(&mut HandFeatures, f32),
) {
    let vals: Vec<f32> = idx
        .iter()
        .map(|&i| get(&rows[i].hand))
        .filter(|v| v.is_finite())
        .collect();
    if vals.len() < 2 {
        return;
    }
    let denom = (vals.len() - 1) as f32;
    for &i in idx {
        let v = get(&rows[i].hand);
        if !v.is_finite() {
            continue;
        }
        // Ties share a rank (count of strictly-better peers), so a burst of identical values is
        // uniformly 0 rather than arbitrarily ordered.
        let better = vals
            .iter()
            .filter(|&&o| if invert { o < v } else { o > v })
            .count();
        set(&mut rows[i].hand, 1.0 - better as f32 / denom);
    }
}

// ── labels + provenance ──────────────────────────────────────────────────────

/// The most recent cull event for one image — everything needed to judge what its label is worth.
#[derive(Debug, Clone, Default)]
struct FlagEvent {
    suggester_id: Option<String>,
    suggestion_score: Option<f64>,
    candidate_ids: Option<String>,
    latency_ms: Option<i64>,
    context: Option<String>,
}

/// Latest `culling.flag_*` event per image.
///
/// SQLite's documented bare-column rule makes the non-aggregated columns come from the row that
/// produced `MAX(id)`, so one grouped pass replaces a per-image correlated subquery.
fn flag_events(conn: &Connection) -> Result<HashMap<i64, FlagEvent>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT image_id, MAX(id), suggester_id, suggestion_score, candidate_ids, latency_ms, context
           FROM user_events
          WHERE event_type IN ('culling.flag_pick','culling.flag_reject') AND image_id IS NOT NULL
          GROUP BY image_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            FlagEvent {
                suggester_id: r.get(2)?,
                suggestion_score: r.get(3)?,
                candidate_ids: r.get(4)?,
                latency_ms: r.get(5)?,
                context: r.get(6)?,
            },
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, ev) = row?;
        out.insert(id, ev);
    }
    Ok(out)
}

/// What the badge said when the user acted: the UI records it in `context.suggested`; a row written
/// before that (or by a path that only had the score) falls back to the score's own side.
fn shown_pick(ev: &FlagEvent) -> bool {
    ev.context
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| match v.get("suggested").and_then(|s| s.as_str()) {
            Some("pick") => Some(true),
            Some("reject") => Some(false),
            _ => None,
        })
        .unwrap_or_else(|| ev.suggestion_score.unwrap_or(0.0) >= 0.5)
}

/// How much this label is worth, from what was on screen when the user made it.
fn classify(y: bool, ev: Option<&FlagEvent>) -> LabelProvenance {
    // No event at all (a label from a sidecar, or from a build before the log): nothing prompted it.
    let Some(ev) = ev else {
        return LabelProvenance::Unprompted;
    };
    // Bulk first: a multi-select flag is keyboard work, not judgement, even if a badge happened to be
    // on screen. The single-image path always records `latency_ms`; the multi-select path never does.
    let bulk = ev.latency_ms.is_none()
        && ev
            .candidate_ids
            .as_deref()
            .and_then(|s| serde_json::from_str::<Vec<i64>>(s).ok())
            .is_some_and(|ids| ids.len() > 1);
    if bulk {
        return LabelProvenance::Batch;
    }
    if ev.suggester_id.is_none() {
        return LabelProvenance::Unprompted;
    }
    if shown_pick(ev) != y {
        return LabelProvenance::Override;
    }
    // An agreement with a confident suggestion carries almost no information; near the decision
    // boundary it does. A missing score is treated as the confident (cheaper) case.
    match ev.suggestion_score {
        Some(s) if (0.35..=0.65).contains(&s) => LabelProvenance::AgreeLo,
        _ => LabelProvenance::AgreeHi,
    }
}

/// Every label in the trainable universe: `(image_id, is_pick, provenance)` in image-id order.
fn labeled_rows(
    conn: &Connection,
    embedding_tag: &str,
) -> Result<Vec<(i64, bool, LabelProvenance)>, LibError> {
    let events = flag_events(conn)?;
    let mut stmt = conn.prepare(
        "SELECT r.image_id, r.flag
           FROM ratings_flags r
           JOIN images i ON i.id = r.image_id
           JOIN image_embedding e ON e.image_id = r.image_id
          WHERE i.status = 'present' AND e.model_tag = :tag AND r.flag IN ('pick','reject')
          ORDER BY r.image_id",
    )?;
    let rows = stmt.query_map(named_params! { ":tag": embedding_tag }, |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)? == "pick"))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, y) = row?;
        out.push((id, y, classify(y, events.get(&id))));
    }
    Ok(out)
}

/// Decode a stored embedding BLOB (little-endian f32, as `image_embedding` holds it).
fn f32_from_le(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// The training set: one [`Sample`] per labeled image that has an embedding from `embedding_tag`,
/// plus the image ids in the same order.
pub fn assemble_samples(
    conn: &Connection,
    embedding_tag: &str,
) -> Result<(Vec<Sample>, Vec<i64>), LibError> {
    let hands: HashMap<i64, HandRow> = load_hand_rows(conn, embedding_tag)?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    let mut vectors = conn.prepare("SELECT vector FROM image_embedding WHERE image_id = ?1")?;
    let (mut samples, mut ids) = (Vec::new(), Vec::new());
    for (id, y, provenance) in labeled_rows(conn, embedding_tag)? {
        let Some(row) = hands.get(&id) else { continue };
        let blob: Vec<u8> = vectors.query_row(params![id], |r| r.get(0))?;
        // A stored vector of the wrong width under a matching tag is corruption. Skipping costs one
        // label; failing would make training impossible until the row were repaired by hand.
        let Ok(x) = assemble(&f32_from_le(&blob), &row.hand) else {
            continue;
        };
        samples.push(Sample {
            x,
            y,
            provenance,
            group: row.group,
        });
        ids.push(id);
    }
    Ok((samples, ids))
}

// ── training + promotion ─────────────────────────────────────────────────────

/// What one training run produced, and whether it took over.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrainOutcome {
    pub model_id: i64,
    pub promoted: bool,
    pub n_pos: usize,
    pub n_neg: usize,
    pub cv_auc: f32,
    pub cv_auprc: f32,
    pub top1_agreement: Option<f32>,
    pub best_lambda: f32,
}

/// Fit on every label, append the model, and promote it if it holds up against the incumbent.
pub fn train_and_store(
    conn: &Connection,
    embedding_tag: &str,
    now_ms: i64,
) -> Result<TrainOutcome, LibError> {
    train_with_grid(conn, embedding_tag, now_ms, CV_FOLDS, &[])
}

/// [`train_and_store`] with an explicit fold count / λ grid (tests keep both small; an empty grid is
/// `core-suggest`'s default sweep).
fn train_with_grid(
    conn: &Connection,
    embedding_tag: &str,
    now_ms: i64,
    k: usize,
    lambdas: &[f32],
) -> Result<TrainOutcome, LibError> {
    let (samples, _) = assemble_samples(conn, embedding_tag)?;
    // Warm-starting from a model fit on a DIFFERENT encoder would seed garbage: same width, other
    // space. `core_suggest::train` already guards feature-version/width; the tag is ours to check.
    let warm = current_model(conn)?
        .map(|(_, m)| m)
        .filter(|m| m.embedding_model_tag == embedding_tag);
    let (model, report) =
        core_suggest::train(&samples, k, lambdas, warm.as_ref(), embedding_tag, now_ms)
            .map_err(|e| LibError::Other(e.to_string()))?;

    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO suggestion_model
             (created_at, model_json, feature_version, embedding_model_tag, n_pos, n_neg,
              cv_auc, cv_auprc, top1_agreement)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            now_ms,
            model.to_json()?,
            model.feature_version,
            embedding_tag,
            model.n_pos as i64,
            model.n_neg as i64,
            model.cv_auc as f64,
            model.cv_auprc as f64,
            model.top1_agreement.map(|v| v as f64),
        ],
    )?;
    let model_id = tx.last_insert_rowid();
    let promoted = promotable(&tx, model.cv_auprc)?;
    if promoted {
        tx.execute(
            "UPDATE suggestion_model SET promoted = 1 WHERE id = ?1",
            params![model_id],
        )?;
        set_meta(&tx, KEY_CURRENT_MODEL, &model_id.to_string())?;
    }
    tx.commit()?;

    Ok(TrainOutcome {
        model_id,
        promoted,
        n_pos: model.n_pos,
        n_neg: model.n_neg,
        cv_auc: model.cv_auc,
        cv_auprc: model.cv_auprc,
        top1_agreement: model.top1_agreement,
        best_lambda: report.best_lambda,
    })
}

/// May a fresh fit take over? Something always beats nothing; after that it may lose at most
/// [`PROMOTE_AUPRC_SLACK`] of out-of-fold AUPRC against the live model.
fn promotable(conn: &Connection, cv_auprc: f32) -> Result<bool, LibError> {
    let Some(id) = current_model_id(conn)? else {
        return Ok(true);
    };
    let incumbent: Option<f64> = conn
        .query_row(
            "SELECT cv_auprc FROM suggestion_model WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    // A dangling pointer defends nothing.
    Ok(incumbent.is_none_or(|prev| cv_auprc as f64 >= prev - PROMOTE_AUPRC_SLACK as f64))
}

fn current_model_id(conn: &Connection) -> Result<Option<i64>, LibError> {
    Ok(get_meta(conn, KEY_CURRENT_MODEL)?.and_then(|v| v.parse().ok()))
}

/// The live model, or `None` when nothing has been promoted (or the pointer dangles).
pub fn current_model(conn: &Connection) -> Result<Option<(i64, Model)>, LibError> {
    let Some(id) = current_model_id(conn)? else {
        return Ok(None);
    };
    let json: Option<String> = conn
        .query_row(
            "SELECT model_json FROM suggestion_model WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .optional()?;
    match json {
        Some(j) => Ok(Some((id, Model::from_json(&j)?))),
        None => Ok(None),
    }
}

// ── scoring ──────────────────────────────────────────────────────────────────

/// Which badge (if any) a score earns.
///
/// The reject side thresholds `1 − p` against a precision-floored threshold; when no operating point
/// met that floor `core-suggest` reports [`core_suggest::TAU_UNREACHABLE`] (2.0), above any
/// probability. The explicit `<= 1.0` guard states that intent rather than relying on the arithmetic.
fn suggested_for(model: &Model, score: f32) -> &'static str {
    if score >= model.tau {
        "pick"
    } else if model.tau_reject <= 1.0 && (1.0 - score) >= model.tau_reject {
        "reject"
    } else {
        "none"
    }
}

/// Deterministic ~[`WITHHELD_PERCENT`]% holdout (FNV-1a over image + model id). Same image, same
/// model → same answer, so a rescoring pass cannot quietly reshuffle which labels stay uninfluenced.
fn withheld(image_id: i64, model_id: i64) -> bool {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in image_id
        .to_le_bytes()
        .iter()
        .chain(model_id.to_le_bytes().iter())
    {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h % 100 < WITHHELD_PERCENT
}

fn flush_scores(
    conn: &Connection,
    model: &Model,
    model_id: i64,
    batch: &[(i64, f32)],
    now_ms: i64,
) -> Result<usize, LibError> {
    if batch.is_empty() {
        return Ok(0);
    }
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR REPLACE INTO image_suggestion
                 (image_id, model_id, score, suggested, withheld, scored_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
        )?;
        for &(id, score) in batch {
            stmt.execute(params![
                id,
                model_id,
                score as f64,
                suggested_for(model, score),
                withheld(id, model_id) as i64,
                now_ms,
            ])?;
        }
    }
    tx.commit()?;
    Ok(batch.len())
}

/// Score every present image carrying an embedding from `embedding_tag` with the live model.
/// Returns the number of rows written.
pub fn score_all(conn: &Connection, embedding_tag: &str, now_ms: i64) -> Result<usize, LibError> {
    let Some((model_id, model)) = current_model(conn)? else {
        return Err(LibError::Other("no promoted suggestion model".into()));
    };
    // Both of these make the stored weights meaningless rather than merely worse — refuse, retrain.
    if model.feature_version != core_suggest::FEATURE_VERSION {
        return Err(LibError::Other(format!(
            "suggestion model {model_id} was fit on feature version {} (runtime {}) — retrain",
            model.feature_version,
            core_suggest::FEATURE_VERSION
        )));
    }
    if model.embedding_model_tag != embedding_tag {
        return Err(LibError::Other(format!(
            "suggestion model {model_id} was fit on {} embeddings (runtime {embedding_tag}) — retrain",
            model.embedding_model_tag
        )));
    }

    let hands: HashMap<i64, HandRow> = load_hand_rows(conn, embedding_tag)?
        .into_iter()
        .map(|r| (r.id, r))
        .collect();
    let mut stmt = conn.prepare(
        "SELECT e.image_id, e.vector
           FROM image_embedding e JOIN images i ON i.id = e.image_id
          WHERE i.status = 'present' AND e.model_tag = :tag
          ORDER BY e.image_id",
    )?;
    let mut rows = stmt.query(named_params! { ":tag": embedding_tag })?;
    let mut batch: Vec<(i64, f32)> = Vec::with_capacity(SCORE_BATCH);
    let mut written = 0usize;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let Some(row) = hands.get(&id) else { continue };
        let Ok(score) = model.score(&f32_from_le(&r.get::<_, Vec<u8>>(1)?), &row.hand) else {
            continue;
        };
        batch.push((id, score));
        if batch.len() == SCORE_BATCH {
            written += flush_scores(conn, &model, model_id, &batch, now_ms)?;
            batch.clear();
        }
    }
    written += flush_scores(conn, &model, model_id, &batch, now_ms)?;

    // Rows still carrying an older model survive only for images this pass could not score (an
    // embedding was removed, or the encoder changed under them). Nothing would ever refresh them, so
    // they would keep showing a badge from weights that are no longer in use.
    conn.execute(
        "DELETE FROM image_suggestion WHERE model_id <> ?1",
        params![model_id],
    )?;
    Ok(written)
}

// ── status ───────────────────────────────────────────────────────────────────

/// Label census over the scorable universe, broken down by how much each label is worth.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCounts {
    pub picks: i64,
    pub rejects: i64,
    pub unprompted: i64,
    pub overrides: i64,
    pub agree_lo: i64,
    pub agree_hi: i64,
    /// Bulk actions. Recorded, never trained on — hence excluded from `trainable`/`delta`.
    pub batch: i64,
}

/// Everything the UI (and the auto-retrain trigger) needs to know about the suggester.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestStatus {
    pub model_id: Option<i64>,
    pub trained_at: Option<i64>,
    pub embedding_model_tag: Option<String>,
    pub cv_auc: Option<f32>,
    pub cv_auprc: Option<f32>,
    pub top1_agreement: Option<f32>,
    /// Labels the live model was fit on — compare against `labels` for how stale it is.
    pub trained_pos: Option<i64>,
    pub trained_neg: Option<i64>,
    pub labels: LabelCounts,
    /// Present images carrying an embedding from the running encoder (the scorable universe).
    pub embedded: i64,
    pub scored: i64,
    pub withheld: i64,
    /// Trainable labels gained (negative: lost) since the live model was fit.
    pub labels_delta: i64,
    /// Enough labels per class to fit at all (`core_suggest::MIN_PER_CLASS`).
    pub trainable: bool,
    /// Set by the IPC layer from the app's run guard — the catalog knows nothing about it.
    pub running: bool,
}

fn count(conn: &Connection, sql: &str, tag: &str) -> Result<i64, LibError> {
    Ok(conn.query_row(sql, named_params! { ":tag": tag }, |r| r.get(0))?)
}

/// Current suggester state: the live model's metrics, the label census, and the scored counts.
pub fn suggest_status(conn: &Connection, embedding_tag: &str) -> Result<SuggestStatus, LibError> {
    let labeled = labeled_rows(conn, embedding_tag)?;
    let mut labels = LabelCounts::default();
    for (_, y, p) in &labeled {
        if *y {
            labels.picks += 1;
        } else {
            labels.rejects += 1;
        }
        match p {
            LabelProvenance::Unprompted => labels.unprompted += 1,
            LabelProvenance::Override => labels.overrides += 1,
            LabelProvenance::AgreeLo => labels.agree_lo += 1,
            LabelProvenance::AgreeHi => labels.agree_hi += 1,
            LabelProvenance::Batch => labels.batch += 1,
        }
    }
    let trainable_count = |pick: bool| {
        labeled
            .iter()
            .filter(|(_, y, p)| *y == pick && *p != LabelProvenance::Batch)
            .count() as i64
    };
    let (pos, neg) = (trainable_count(true), trainable_count(false));

    let model = current_model(conn)?;
    let mut st = SuggestStatus {
        model_id: model.as_ref().map(|(id, _)| *id),
        trained_at: model.as_ref().map(|(_, m)| m.trained_at_ms),
        embedding_model_tag: model.as_ref().map(|(_, m)| m.embedding_model_tag.clone()),
        cv_auc: model.as_ref().map(|(_, m)| m.cv_auc),
        cv_auprc: model.as_ref().map(|(_, m)| m.cv_auprc),
        top1_agreement: model.as_ref().and_then(|(_, m)| m.top1_agreement),
        trained_pos: model.as_ref().map(|(_, m)| m.n_pos as i64),
        trained_neg: model.as_ref().map(|(_, m)| m.n_neg as i64),
        labels,
        embedded: count(
            conn,
            "SELECT COUNT(*) FROM image_embedding e JOIN images i ON i.id = e.image_id
              WHERE i.status = 'present' AND e.model_tag = :tag",
            embedding_tag,
        )?,
        scored: conn.query_row("SELECT COUNT(*) FROM image_suggestion", [], |r| r.get(0))?,
        withheld: conn.query_row(
            "SELECT COUNT(*) FROM image_suggestion WHERE withheld = 1",
            [],
            |r| r.get(0),
        )?,
        labels_delta: 0,
        trainable: pos >= core_suggest::MIN_PER_CLASS as i64
            && neg >= core_suggest::MIN_PER_CLASS as i64,
        running: false,
    };
    // Compared against the model's own (Batch-free) counts, so the delta is 0 right after a fit.
    st.labels_delta = pos + neg - st.trained_pos.unwrap_or(0) - st.trained_neg.unwrap_or(0);
    Ok(st)
}

/// Trainable labels gained (negative: lost) since the live model was fit. `0` means the model has
/// seen everything there is; with no model at all this is the full label count.
pub fn label_count_delta(conn: &Connection, embedding_tag: &str) -> Result<i64, LibError> {
    Ok(suggest_status(conn, embedding_tag)?.labels_delta)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::Db;
    use core_suggest::EMB_DIM;

    const TAG: &str = "mobileclip-s1-v1";

    fn img(conn: &Connection, tag: i64, filename: &str, fingerprint: Option<&[u8]>) -> i64 {
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                 capture_fingerprint, iso, shutter, aperture, focal_length, format, imported_at)
             VALUES (?1, 1, ?2, ?3, 'present', ?4, 400, '1/250', 2.8, 50.0, 'raw', 0)",
            params![
                vec![tag as u8; 32],
                format!("/lib/{filename}"),
                filename,
                fingerprint,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// Store an embedding whose first component carries the (linearly separable) signal.
    fn embed(conn: &Connection, id: i64, signal: f32, tag: &str) {
        let mut v = vec![0.0f32; EMB_DIM];
        v[0] = signal;
        v[1] = signal * 0.05;
        let blob: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        conn.execute(
            "INSERT OR REPLACE INTO image_embedding(image_id, dim, vector, model_tag, computed_at)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![id, EMB_DIM as i64, blob, tag],
        )
        .unwrap();
    }

    fn flag(conn: &Connection, id: i64, flag: &str) {
        crate::cull::set_flag(conn, id, flag).unwrap();
    }

    /// Append one `culling.flag_*` event (only the fields provenance depends on).
    #[allow(clippy::too_many_arguments)]
    fn event(
        conn: &Connection,
        id: i64,
        pick: bool,
        suggester: Option<&str>,
        score: Option<f64>,
        candidates: Option<&str>,
        latency: Option<i64>,
        context: Option<&str>,
    ) {
        crate::events::append_event(
            conn,
            &crate::events::Event {
                ts_ms: 0,
                session_id: "s".into(),
                app_version: "t".into(),
                suggester_id: suggester.map(str::to_string),
                event_type: if pick {
                    "culling.flag_pick".into()
                } else {
                    "culling.flag_reject".into()
                },
                image_id: Some(id),
                candidate_ids: candidates.map(str::to_string),
                suggestion_score: score,
                latency_ms: latency,
                context: context.map(str::to_string),
                ..Default::default()
            },
        )
        .unwrap();
    }

    fn provenance_of(conn: &Connection, id: i64) -> LabelProvenance {
        labeled_rows(conn, TAG)
            .unwrap()
            .into_iter()
            .find(|(i, _, _)| *i == id)
            .map(|(_, _, p)| p)
            .expect("labeled row")
    }

    #[test]
    fn provenance_covers_every_way_a_label_can_be_made() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        let mk = |name: &str, pick: bool| {
            let id = img(c, 1, name, None);
            embed(c, id, 0.0, TAG);
            flag(c, id, if pick { "pick" } else { "reject" });
            id
        };

        // No event at all, and an event with no suggester on screen: both unprompted.
        let silent = mk("A0001.cr3", true);
        let no_suggester = mk("B0001.cr3", true);
        event(c, no_suggester, true, None, None, None, Some(900), None);
        // Agreement, near the boundary vs. confident.
        let agree_lo = mk("C0001.cr3", true);
        event(
            c,
            agree_lo,
            true,
            Some("m1"),
            Some(0.55),
            None,
            Some(700),
            None,
        );
        let agree_hi = mk("D0001.cr3", true);
        event(
            c,
            agree_hi,
            true,
            Some("m1"),
            Some(0.95),
            None,
            Some(700),
            None,
        );
        // Contradiction: a confident "pick" badge, user rejected.
        let override_ = mk("E0001.cr3", false);
        event(
            c,
            override_,
            false,
            Some("m1"),
            Some(0.93),
            None,
            Some(700),
            None,
        );
        // Multi-select: several candidates and no decision latency.
        let batch = mk("F0001.cr3", false);
        event(
            c,
            batch,
            false,
            Some("m1"),
            Some(0.9),
            Some("[1,2,3]"),
            None,
            None,
        );
        // `context.suggested` beats the score: badge said "reject" despite a 0.9 score, user rejected.
        let ctx_agree = mk("G0001.cr3", false);
        event(
            c,
            ctx_agree,
            false,
            Some("m1"),
            Some(0.9),
            None,
            Some(400),
            Some(r#"{"suggested":"reject"}"#),
        );

        assert_eq!(provenance_of(c, silent), LabelProvenance::Unprompted);
        assert_eq!(provenance_of(c, no_suggester), LabelProvenance::Unprompted);
        assert_eq!(provenance_of(c, agree_lo), LabelProvenance::AgreeLo);
        assert_eq!(provenance_of(c, agree_hi), LabelProvenance::AgreeHi);
        assert_eq!(provenance_of(c, override_), LabelProvenance::Override);
        assert_eq!(
            provenance_of(c, batch),
            LabelProvenance::Batch,
            "a bulk action must never train, whatever was on screen"
        );
        assert_eq!(provenance_of(c, ctx_agree), LabelProvenance::AgreeHi);

        // A single-image action on a one-item candidate list is NOT a batch.
        let single = mk("H0001.cr3", true);
        event(
            c,
            single,
            true,
            Some("m1"),
            Some(0.9),
            Some("[7]"),
            Some(120),
            None,
        );
        assert_eq!(provenance_of(c, single), LabelProvenance::AgreeHi);
    }

    /// The IPC layer writes `context` through `events::context_with_suggested`; `classify` reads it
    /// back. Exercising the pair together keeps the writer and the reader from drifting apart.
    #[test]
    fn a_context_written_by_the_helper_reads_back_as_provenance() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        let ctx = |shown: &str| crate::events::context_with_suggested(None, Some(shown));

        // Badge said "pick", user picked → agreement (score decides how much it is worth).
        let agreed = img(c, 1, "J0001.cr3", None);
        embed(c, agreed, 0.0, TAG);
        flag(c, agreed, "pick");
        event(
            c,
            agreed,
            true,
            Some("model"),
            Some(0.91),
            None,
            Some(250),
            ctx("pick").as_deref(),
        );
        assert_eq!(provenance_of(c, agreed), LabelProvenance::AgreeHi);

        // Badge said "reject", user picked anyway → the informative case.
        let contradicted = img(c, 2, "K0001.cr3", None);
        embed(c, contradicted, 0.0, TAG);
        flag(c, contradicted, "pick");
        event(
            c,
            contradicted,
            true,
            Some("model"),
            Some(0.08),
            None,
            Some(250),
            ctx("reject").as_deref(),
        );
        assert_eq!(provenance_of(c, contradicted), LabelProvenance::Override);
    }

    #[test]
    fn the_latest_event_decides_the_provenance() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        let id = img(c, 1, "I0001.cr3", None);
        embed(c, id, 0.0, TAG);
        flag(c, id, "pick");
        // First rejected unprompted, then re-picked against a confident "reject" badge.
        event(c, id, false, None, None, None, Some(300), None);
        event(
            c,
            id,
            true,
            Some("m1"),
            Some(0.05),
            None,
            Some(300),
            Some(r#"{"suggested":"reject"}"#),
        );
        assert_eq!(provenance_of(c, id), LabelProvenance::Override);
    }

    #[test]
    fn bursts_group_by_fingerprint_then_by_filename() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        // Two frames of one capture (RAW + camera JPEG share a fingerprint), plus a lone capture.
        let raw = img(c, 1, "R0001.cr3", Some(b"fp-a"));
        let jpg = img(c, 2, "R0001.jpg", Some(b"fp-a"));
        let other = img(c, 3, "R0009.cr3", Some(b"fp-b"));
        // No fingerprint: consecutive frame numbers cluster, a distant one does not.
        let seq1 = img(c, 4, "IMG_0100.cr3", None);
        let seq2 = img(c, 5, "IMG_0103.cr3", None);
        let far = img(c, 6, "IMG_0400.cr3", None);
        let unparseable = img(c, 7, "scan.tif", None);
        for id in [raw, jpg, other, seq1, seq2, far, unparseable] {
            embed(c, id, 0.0, TAG);
        }

        let rows = load_hand_rows(c, TAG).unwrap();
        let g = |id: i64| rows.iter().find(|r| r.id == id).unwrap().group;
        assert_eq!(g(raw), g(jpg), "one capture is one group");
        assert_ne!(g(raw), g(other));
        assert_eq!(g(seq1), g(seq2), "frames 3 apart are the same burst");
        assert_ne!(g(seq1), g(far), "a 300-frame gap is a different burst");
        for id in [other, far, unparseable] {
            assert_eq!(
                rows.iter().filter(|r| r.group == g(id)).count(),
                1,
                "a lone frame must not share a group id"
            );
        }
    }

    #[test]
    fn burst_ranks_are_relative_and_iso_is_inverted() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        let ids: Vec<i64> = (0..3)
            .map(|i| {
                let id = img(c, i, &format!("IMG_010{i}.cr3"), None);
                embed(c, id, 0.0, TAG);
                c.execute(
                    "UPDATE images SET iso = ?2 WHERE id = ?1",
                    params![id, 100 * (i + 1)],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO image_features(image_id, sharpness, computed_at)
                     VALUES (?1, ?2, 0)",
                    params![id, (i as f64 + 1.0) * 100.0],
                )
                .unwrap();
                id
            })
            .collect();
        let lone = img(c, 9, "solo.tif", None);
        embed(c, lone, 0.0, TAG);

        let rows = load_hand_rows(c, TAG).unwrap();
        let h = |id: i64| rows.iter().find(|r| r.id == id).unwrap().hand;
        assert_eq!(h(ids[2]).rank_sharpness, 1.0, "sharpest frame ranks top");
        assert_eq!(h(ids[0]).rank_sharpness, 0.0);
        assert_eq!(h(ids[1]).rank_sharpness, 0.5);
        assert_eq!(h(ids[0]).rank_iso, 1.0, "lowest ISO ranks top (inverted)");
        assert_eq!(h(ids[2]).rank_iso, 0.0);
        // Nothing to compare against → neutral, and a missing signal stays unknown, not zero.
        assert_eq!(h(lone).rank_sharpness, 0.5);
        assert!(h(lone).sharpness_log.is_nan());
        assert!(h(lone).face_max_quality.is_nan());
        assert_eq!(h(lone).face_count, 0.0);
    }

    /// 12 picks + 12 rejects, linearly separable on the embedding's first component.
    fn seed_labeled_library(conn: &Connection) {
        for i in 0..24i64 {
            // NOT alternating: folds are dealt round-robin over the (singleton) groups, so an
            // alternating label would put one whole class in each fold and train on nothing.
            let pick = i < 12;
            let id = img(conn, i, &format!("IMG_{:04}.cr3", 1000 + i * 50), None);
            embed(conn, id, if pick { 1.0 } else { -1.0 }, TAG);
            flag(conn, id, if pick { "pick" } else { "reject" });
        }
    }

    fn train_small(conn: &Connection) -> TrainOutcome {
        train_with_grid(conn, TAG, 1_754_000_000_000, 2, &[1e-2]).unwrap()
    }

    #[test]
    fn the_first_model_is_promoted_and_a_worse_retrain_is_not() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        seed_labeled_library(c);

        let first = train_small(c);
        assert!(first.promoted, "something must beat nothing");
        assert_eq!((first.n_pos, first.n_neg), (12, 12));
        assert!(first.cv_auc > 0.9, "separable data: {}", first.cv_auc);
        assert_eq!(current_model_id(c).unwrap(), Some(first.model_id));

        // Re-embed so the signal no longer tracks the label (it now alternates with the image id,
        // which is balanced across both classes): the next fit sees the same labels over features
        // that cannot separate them, so it scores at chance — a real "the retrain got worse" case.
        for (id, _, _) in labeled_rows(c, TAG).unwrap() {
            embed(c, id, if id % 2 == 0 { 1.0 } else { -1.0 }, TAG);
        }
        let second = train_small(c);
        assert!(
            second.cv_auprc < first.cv_auprc - PROMOTE_AUPRC_SLACK,
            "test setup: {} vs {}",
            second.cv_auprc,
            first.cv_auprc
        );
        assert!(
            !second.promoted,
            "a materially worse fit must not take the badges over"
        );
        assert_eq!(
            current_model_id(c).unwrap(),
            Some(first.model_id),
            "the pointer must still name the defended model"
        );
        // ...but the attempt is still on record.
        let rows: i64 = c
            .query_row("SELECT COUNT(*) FROM suggestion_model", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2);
        let promoted: i64 = c
            .query_row(
                "SELECT promoted FROM suggestion_model WHERE id = ?1",
                params![second.model_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(promoted, 0);
    }

    #[test]
    fn scoring_writes_a_badge_per_image_and_clears_stale_rows() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        seed_labeled_library(c);
        // An unlabeled image gets scored too — that is the whole point.
        let fresh = img(c, 99, "IMG_9999.cr3", None);
        embed(c, fresh, 1.0, TAG);
        let outcome = train_small(c);

        // A leftover row from an older model, on an image this pass cannot score (no embedding).
        let orphan = img(c, 98, "IMG_9998.cr3", None);
        c.execute(
            "INSERT INTO image_suggestion(image_id, model_id, score, suggested, scored_at)
             VALUES (?1, ?2, 0.5, 'pick', 0)",
            params![orphan, outcome.model_id],
        )
        .unwrap();
        c.execute(
            "UPDATE image_suggestion SET model_id = ?2 WHERE image_id = ?1",
            params![orphan, outcome.model_id],
        )
        .unwrap();
        c.execute(
            "INSERT INTO suggestion_model
                 (created_at, model_json, feature_version, embedding_model_tag, n_pos, n_neg,
                  cv_auc, cv_auprc)
             VALUES (0, '{}', 1, ?1, 1, 1, 0.5, 0.5)",
            params![TAG],
        )
        .unwrap();
        let stale_model = c.last_insert_rowid();
        c.execute(
            "UPDATE image_suggestion SET model_id = ?2 WHERE image_id = ?1",
            params![orphan, stale_model],
        )
        .unwrap();

        let n = score_all(c, TAG, 1_754_000_100_000).unwrap();
        assert_eq!(n, 25, "every embedded present image is scored");
        let stale: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM image_suggestion WHERE model_id <> ?1",
                params![outcome.model_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0, "a row from a superseded model must not survive");

        // The separable seed must actually produce picks, and every value is a legal badge.
        let picks: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM image_suggestion WHERE suggested = 'pick'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(picks > 0, "a model that learned nothing suggests nothing");

        // Withheld is stable across passes (deterministic), never everything.
        let snapshot = |conn: &Connection| -> Vec<(i64, i64)> {
            let mut stmt = conn
                .prepare("SELECT image_id, withheld FROM image_suggestion ORDER BY image_id")
                .unwrap();
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            rows
        };
        let before = snapshot(c);
        score_all(c, TAG, 1_754_000_200_000).unwrap();
        assert_eq!(before, snapshot(c), "the holdout must not reshuffle");
        assert!(before.iter().filter(|(_, w)| *w == 1).count() < before.len());
    }

    #[test]
    fn scoring_refuses_a_model_from_another_encoder() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        seed_labeled_library(c);
        train_small(c);
        let err = score_all(c, "some-other-encoder-v2", 0).unwrap_err();
        assert!(
            err.to_string().contains("retrain"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn an_unreachable_reject_threshold_never_fires() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        seed_labeled_library(c);
        let outcome = train_small(c);
        let (_, mut model) = current_model(c).unwrap().unwrap();

        // The sentinel `core-suggest` reports when no operating point met the precision floor.
        model.tau_reject = core_suggest::TAU_UNREACHABLE;
        model.tau = 2.0;
        for score in [0.0, 0.01, 0.5, 0.99, 1.0] {
            assert_eq!(suggested_for(&model, score), "none", "score {score}");
        }
        // A reachable threshold does fire on the same scores.
        model.tau_reject = 0.9;
        assert_eq!(suggested_for(&model, 0.05), "reject");

        c.execute(
            "UPDATE suggestion_model SET model_json = ?2 WHERE id = ?1",
            params![outcome.model_id, {
                let mut m = model.clone();
                m.tau_reject = core_suggest::TAU_UNREACHABLE;
                m.tau = 2.0;
                m.to_json().unwrap()
            }],
        )
        .unwrap();
        score_all(c, TAG, 0).unwrap();
        let non_none: i64 = c
            .query_row(
                "SELECT COUNT(*) FROM image_suggestion WHERE suggested <> 'none'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(non_none, 0);
    }

    #[test]
    fn status_counts_labels_by_provenance_and_tracks_drift() {
        let db = Db::open_in_memory().unwrap();
        let c = &db.conn;
        seed_labeled_library(c);
        // One of the picks was a bulk action: counted, but never trained on.
        let bulk = labeled_rows(c, TAG).unwrap()[0].0;
        event(
            c,
            bulk,
            true,
            Some("m1"),
            Some(0.9),
            Some("[1,2]"),
            None,
            None,
        );

        let before = suggest_status(c, TAG).unwrap();
        assert_eq!((before.labels.picks, before.labels.rejects), (12, 12));
        assert_eq!(before.labels.batch, 1);
        assert_eq!(before.labels.unprompted, 23);
        assert_eq!(before.embedded, 24);
        assert!(before.trainable);
        assert_eq!(
            before.labels_delta, 23,
            "with no model, every trainable label is new"
        );
        assert!(before.model_id.is_none());

        let outcome = train_small(c);
        let after = suggest_status(c, TAG).unwrap();
        assert_eq!(after.model_id, Some(outcome.model_id));
        assert_eq!(after.labels_delta, 0, "a fresh fit has seen everything");
        assert_eq!(after.trained_pos, Some(11));
        assert_eq!(after.embedding_model_tag.as_deref(), Some(TAG));
        assert_eq!(label_count_delta(c, TAG).unwrap(), 0);

        // A label on an image with no embedding is invisible to the model (nothing to score it by).
        let unembedded = img(c, 97, "IMG_9997.cr3", None);
        flag(c, unembedded, "pick");
        assert_eq!(label_count_delta(c, TAG).unwrap(), 0);
    }

    #[test]
    fn the_withheld_slice_is_deterministic_and_roughly_eight_percent() {
        let held = (1..=2000i64).filter(|&id| withheld(id, 3)).count();
        assert_eq!(held, (1..=2000i64).filter(|&id| withheld(id, 3)).count());
        assert!(
            (60..=220).contains(&held),
            "expected ~8% of 2000, got {held}"
        );
        // A different model reshuffles the slice (a new model deserves a fresh unbiased sample).
        assert_ne!(
            (1..=2000i64).filter(|&id| withheld(id, 4)).count(),
            0,
            "the holdout must not collapse for some model ids"
        );
    }

    #[test]
    fn shutter_strings_parse_or_stay_unknown() {
        assert_eq!(parse_shutter_seconds("1/250"), Some(1.0 / 250.0));
        assert_eq!(parse_shutter_seconds("2.5s"), Some(2.5));
        assert_eq!(parse_shutter_seconds("0.004"), Some(0.004));
        for bad in ["0", "1/0", "", "bulb", "-1"] {
            assert_eq!(parse_shutter_seconds(bad), None, "{bad}");
        }
    }
}
