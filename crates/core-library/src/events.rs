//! Append-only user-event log — captures decision/label signals for future on-device AI training
//! (dedup keeper choices, cull picks/rejects, edit commits, exports). Writes one immutable row to
//! `user_events` per decision; never updates or deletes. See `007_user_events.sql`.
//!
//! Owned-string fields (vs borrowed) keep construction ergonomic at the IPC layer — events fire at
//! human interaction frequency, so the clones are immaterial.

use core_db::rusqlite::{params, Connection};
use serde::Serialize;

use crate::error::LibError;

/// One user-decision/label fact. Set only the fields relevant to `event_type`; the rest stay `None`.
/// Column set mirrors `user_events` in `007_user_events.sql`.
#[derive(Debug, Default, Clone, Serialize)]
pub struct Event {
    pub ts_ms: i64,
    pub session_id: String,
    pub app_version: String,
    pub process_version: Option<i64>,
    pub suggester_id: Option<String>,
    pub event_type: String,
    pub image_id: Option<i64>,
    pub group_id: Option<String>,
    /// JSON int array — the FULL candidate set shown.
    pub candidate_ids: Option<String>,
    pub chosen_id: Option<i64>,
    /// JSON int array — explicit negatives.
    pub rejected_ids: Option<String>,
    pub suggestion_id: Option<i64>,
    pub suggestion_score: Option<f64>,
    pub params_before: Option<String>,
    pub params_after: Option<String>,
    pub scalar_key: Option<String>,
    pub scalar_before: Option<f64>,
    pub scalar_after: Option<f64>,
    pub stars: Option<i64>,
    pub flag: Option<String>,
    pub color_label: Option<String>,
    pub latency_ms: Option<i64>,
    pub touch_count: Option<i64>,
    pub is_implicit: bool,
    /// JSON catch-all for extra context.
    pub context: Option<String>,
}

/// Append one event (caller supplies `now_ms`; keep within the same tx as the state mutation).
pub fn append_event(conn: &Connection, e: &Event) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO user_events
           (ts, session_id, app_version, process_version, suggester_id, event_type, image_id,
            group_id, candidate_ids, chosen_id, rejected_ids, suggestion_id, suggestion_score,
            params_before, params_after, scalar_key, scalar_before, scalar_after, stars, flag,
            color_label, latency_ms, touch_count, is_implicit, context)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)",
        params![
            e.ts_ms,
            e.session_id,
            e.app_version,
            e.process_version,
            e.suggester_id,
            e.event_type,
            e.image_id,
            e.group_id,
            e.candidate_ids,
            e.chosen_id,
            e.rejected_ids,
            e.suggestion_id,
            e.suggestion_score,
            e.params_before,
            e.params_after,
            e.scalar_key,
            e.scalar_before,
            e.scalar_after,
            e.stars,
            e.flag,
            e.color_label,
            e.latency_ms,
            e.touch_count,
            e.is_implicit as i64,
            e.context,
        ],
    )?;
    Ok(())
}

/// Serialize an id slice to a JSON array string (for `candidate_ids` / `rejected_ids`).
pub fn ids_json(ids: &[i64]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_string())
}

/// Total event count (smoke/verification).
pub fn event_count(conn: &Connection) -> Result<i64, LibError> {
    Ok(conn.query_row("SELECT COUNT(*) FROM user_events", [], |r| r.get(0))?)
}
