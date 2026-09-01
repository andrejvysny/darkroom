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

/// Fold what the suggestion badge said into an event's `context` JSON.
///
/// `suggest::classify` reads `context.suggested` to tell an agreement from an override, so the key
/// has to survive next to whatever else a caller already put in `context` — hence a merge rather
/// than an overwrite. A context that is not a JSON *object* (or does not parse) is replaced: a
/// malformed blob would otherwise swallow the one field provenance depends on. Only the two badge
/// values are honoured; anything else leaves the context untouched and `classify` falls back to the
/// score's own side.
pub fn context_with_suggested(context: Option<String>, suggested: Option<&str>) -> Option<String> {
    let Some(flag @ ("pick" | "reject")) = suggested else {
        return context;
    };
    let mut obj = context
        .as_deref()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default();
    obj.insert(
        "suggested".to_string(),
        serde_json::Value::String(flag.to_string()),
    );
    serde_json::to_string(&serde_json::Value::Object(obj)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggested_key(json: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(json)
            .ok()?
            .get("suggested")?
            .as_str()
            .map(str::to_string)
    }

    #[test]
    fn a_badge_is_recorded_without_losing_the_rest_of_the_context() {
        // Nothing on screen: the context is whatever the caller had (usually nothing).
        assert_eq!(context_with_suggested(None, None), None);
        assert_eq!(
            context_with_suggested(Some(r#"{"a":1}"#.into()), None).as_deref(),
            Some(r#"{"a":1}"#)
        );

        // No context yet → one is created.
        let created = context_with_suggested(None, Some("pick")).unwrap();
        assert_eq!(suggested_key(&created).as_deref(), Some("pick"));

        // An existing object keeps its keys.
        let merged = context_with_suggested(Some(r#"{"a":1}"#.into()), Some("reject")).unwrap();
        assert_eq!(suggested_key(&merged).as_deref(), Some("reject"));
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v.get("a").and_then(|x| x.as_i64()), Some(1));

        // A pre-existing badge is overwritten by the one actually shown.
        let replaced =
            context_with_suggested(Some(r#"{"suggested":"pick"}"#.into()), Some("reject")).unwrap();
        assert_eq!(suggested_key(&replaced).as_deref(), Some("reject"));

        // Junk / non-object context must not hide the key provenance depends on.
        for junk in ["not json", "[1,2]", "\"scalar\""] {
            let out = context_with_suggested(Some(junk.into()), Some("pick")).unwrap();
            assert_eq!(suggested_key(&out).as_deref(), Some("pick"), "{junk}");
        }

        // An unknown badge value is not a badge.
        assert_eq!(context_with_suggested(None, Some("maybe")), None);
    }
}
