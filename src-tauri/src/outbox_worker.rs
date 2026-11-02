//! Tauri-side spawn of the durable sidecar-mirroring worker. The drain itself — the queue walk,
//! backoff, and fenced writes — lives in [`core_runtime::sidecar::outbox`]; this only wires it to
//! `AppState` and the supervisor so both hosts can share the logic.

use crate::state::AppState;
use tauri::{AppHandle, Manager};

/// Spawn the single mirroring worker (same place as the sidecar/thumbnail workers).
pub fn spawn_worker(app: AppHandle) {
    let handle = {
        let st = app.state::<AppState>();
        st.outbox.clone()
    };
    let worker_app = app.clone();
    if let Err(error) = crate::jobs::worker(&app, "outbox-worker", move || {
        let st = worker_app.state::<AppState>();
        handle.run(&st.publications, &|| st.workers.is_stopping(), &|| {});
        Ok(())
    }) {
        tracing::error!(%error, "sidecar outbox worker failed to start");
    }
}
