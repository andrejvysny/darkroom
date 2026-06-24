//! Behavioral-event emission — stamps session/app/time context onto a `core_library::Event` and
//! appends it to the append-only log. Capture is best-effort: a logging failure must never fail the
//! user action it describes.

use core_library::Event;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state::AppState;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Fill the session/app/time context on `e` (call before appending within an existing DB lock).
pub fn stamp(st: &AppState, mut e: Event) -> Event {
    e.ts_ms = now_ms();
    e.session_id = st.session_id.clone();
    e.app_version = st.app_version.to_string();
    e
}

/// Stamp + append `e` under a brief DB lock (for callers NOT already holding the lock). Best-effort.
pub fn log_event(st: &AppState, e: Event) {
    let e = stamp(st, e);
    if let Ok(db) = st.db.lock() {
        if let Err(err) = core_library::append_event(&db.conn, &e) {
            tracing::warn!(event_type = %e.event_type, error = %err, "behavioral event append failed");
        }
    }
}
