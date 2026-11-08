//! The save barrier shared by a normal quit and by an updater relaunch.
//!
//! Develop edits are debounced in the webview before they reach `develop_set_edit`, so at the
//! moment the user presses ⌘Q (or accepts an update) the newest edit can still be sitting in a
//! JavaScript timer. Flushing the WAL on exit cannot recover that — the row was never written. So
//! the exit is held, the frontend is asked to flush, and the caller proceeds only once the frontend
//! has acknowledged a successful save.
//!
//! The coordination (barrier identity, revision-specific acks, timeout, grace) lives in the
//! Tauri-free [`core_runtime::save_barrier`]; this module is the thin adapter that emits the flush
//! request, receives `develop_flush_ack`, and verifies the claimed revisions against the catalog.
//! The coordinator always runs on a worker thread, never on the ordered `UserCommand` lane: the
//! frontend's flush is itself an ordered `develop_set_edit` job and could not start under it.
//!
//! What the frontend owes is an *acknowledgement*, not just a reply:
//! - `ok = true` plus the committed revision of every edit it saved, which the catalog must confirm
//!   → [`BarrierResult::Flushed`]; an unconfirmed claim is [`BarrierResult::Failed`].
//! - `ok = false` → a save failed: [`BarrierResult::Failed`].
//! - no reply within `ACK_TIMEOUT` → [`BarrierResult::NoAcknowledgement`] — the webview is wedged
//!   or gone, and we cannot tell "nothing was pending" from "the newest edit is still in memory".
//!
//! All of these are handled the same way by the caller: only `Flushed` lets the process leave. A
//! timeout cancels the exit the same way a failed flush does: the failure is remembered,
//! `app:save-barrier` is emitted so the UI can offer retry / cancel / quit-anyway, and the app
//! stays open.
//!
//! The escape hatch is deliberately kept: the *next* request within `RETRY_GRACE` proceeds
//! unconditionally. The user has been told the save failed and asked again, which is an explicit
//! decision to discard the in-memory edit — a failing disk must never hold someone hostage in their
//! own editor. It is keyed per [`BarrierReason`]: grace earned by discarding a warned
//! updater-restart never lets a ⌘Q leave with unsaved edits the quit never warned about.
//!
//! Mirroring the committed edits into the sidecars beside the RAWs is *not* part of the barrier:
//! `core_library::save_edit_with_intent` commits the intent in the same transaction as the edit, so
//! an unwritten sidecar is durable work the next launch finishes ([`crate::outbox_worker`]). Quit
//! therefore waits for the frontend, logs what the outbox still owes, and never blocks on it.

use core_runtime::save_barrier::{
    verify_revisions, AckVerdict, BarrierId, Begin, SavedRevision, Ticket, ACK_TIMEOUT,
};
pub use core_runtime::save_barrier::{BarrierReason, BarrierResult};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// Event the frontend listens for; it replies with the `develop_flush_ack` command.
const FLUSH_EVENT: &str = "app:flush-edits";
// `app:save-barrier` is announced when a barrier did not reach disk, so the UI can say why and
// offer the choices (retry, cancel, or ask again to discard). It is emitted by name at the call
// site on purpose: the parity scanner reads event topics out of the `emit` call sites.

/// Payload of `app:flush-edits`: the frontend echoes `barrier_id` back in `develop_flush_ack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FlushRequest {
    barrier_id: u64,
}

/// One edit the frontend claims to have committed, as sent by `develop_flush_ack`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedRevisionArg {
    pub image_id: i64,
    pub revision: i64,
}

/// Deliver `develop_flush_ack`. A mismatched/late `barrier_id` is ignored by the coordinator.
pub fn deliver_ack(app: &AppHandle, barrier_id: u64, ok: bool, saved: Vec<SavedRevisionArg>) {
    let verdict = if ok {
        AckVerdict::ok(
            saved
                .into_iter()
                .map(|s| SavedRevision {
                    image_id: s.image_id,
                    revision: s.revision,
                })
                .collect(),
        )
    } else {
        AckVerdict::failed()
    };
    if !app
        .state::<AppState>()
        .quit
        .ack(BarrierId(barrier_id), verdict)
    {
        tracing::debug!(barrier_id, "ignored a late or stray save-barrier ack");
    }
}

/// Payload of `app:save-barrier`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SaveBarrierReport {
    /// `"quit"` or `"updater-restart"`.
    pub reason: &'static str,
    pub message: String,
}

/// Build the UI report for one ended barrier. Pure, so the reason/outcome mapping is testable
/// without a webview.
fn report_for(reason: BarrierReason, result: &BarrierResult) -> SaveBarrierReport {
    SaveBarrierReport {
        reason: reason.label(),
        message: result.message(reason),
    }
}

/// Ask the frontend to flush its debounced edits and wait for the verdict.
///
/// Blocks: must run on a worker thread — not the main thread (the webview needs the run loop alive
/// to receive `app:flush-edits` and deliver `develop_flush_ack`) and not the ordered `UserCommand`
/// lane (the frontend's flush is an ordered job).
fn coordinate(app: &AppHandle, ticket: Ticket, reason: BarrierReason) -> BarrierResult {
    let st = app.state::<AppState>();
    let barrier = &st.quit;
    let result = barrier.run(
        ticket,
        reason,
        |id| {
            if app.webview_windows().is_empty() {
                // No webview to ask at all (last window closed; macOS keeps the app alive). Nothing
                // is held in editor memory that a flush could still recover, so the ack-forever
                // case must not cancel the exit with a toast nobody can see: proceed immediately.
                // (The emit alone cannot detect this — it succeeds happily with zero listeners.)
                barrier.ack(id, AckVerdict::ok(Vec::new()));
            } else if app
                .emit(FLUSH_EVENT, FlushRequest { barrier_id: id.0 })
                .is_err()
            {
                // Emit failed — the webview never got the ask. Same shape: do not make the caller
                // wait out the timeout for an undeliverable request.
                barrier.ack(id, AckVerdict::ok(Vec::new()));
            }
            Ok(())
        },
        |saved| {
            // After the ack, briefly and off the lane: confirm the catalog holds what was claimed.
            let db = st.db.lock().map_err(|e| e.to_string())?;
            verify_revisions(&db.conn, saved)
        },
        ACK_TIMEOUT,
    );
    let outbox = st.outbox.pending_report();
    match &result {
        BarrierResult::Flushed => tracing::info!(
            reason = reason.verb(),
            outbox_pending = outbox.0,
            outbox_failing = outbox.1,
            outbox_last_error = outbox.2.as_deref().unwrap_or(""),
            "save barrier flushed; sidecar intents stay queued and durable across the restart"
        ),
        failed => {
            tracing::warn!(
                reason = reason.verb(),
                outbox_pending = outbox.0,
                outbox_failing = outbox.1,
                outbox_last_error = outbox.2.as_deref().unwrap_or(""),
                message = %failed.message(reason),
                "save barrier did not reach disk; the exit is cancelled"
            );
            let _ = app.emit("app:save-barrier", &report_for(reason, failed));
        }
    }
    result
}

/// Run the save barrier for `reason` on a worker thread and await its result without blocking the
/// caller's executor thread. The caller decides what a non-`Flushed` result means for it.
pub async fn run_save_barrier(app: &AppHandle, reason: BarrierReason) -> BarrierResult {
    let ticket = match app.state::<AppState>().quit.begin(reason) {
        // The grace window: the user already chose to discard.
        Begin::Proceed => return BarrierResult::Flushed,
        Begin::Hold => {
            return BarrierResult::Failed(
                "A save is already in progress; try again once it finishes.".to_string(),
            )
        }
        Begin::Start(ticket) => ticket,
    };
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let barrier_app = app.clone();
    if let Err(error) = crate::jobs::worker(app, "restart-flush", move || {
        let result = coordinate(&barrier_app, ticket, reason);
        let _ = tx.blocking_send(result);
        Ok(())
    }) {
        return abandon(app, reason, format!("save coordinator: {error}"));
    }
    match rx.recv().await {
        Some(result) => result,
        None => abandon(
            app,
            reason,
            "save coordinator ended unexpectedly".to_string(),
        ),
    }
}

/// A claimed barrier that will never run: release the claim as a failure.
fn abandon(app: &AppHandle, reason: BarrierReason, message: String) -> BarrierResult {
    let result = BarrierResult::Failed(message);
    app.state::<AppState>().quit.settle(reason, &result);
    result
}

/// Handle `RunEvent::ExitRequested`. Returns `true` when the caller must call `api.prevent_exit()`.
///
/// `code` is `Some` for a programmatic exit — including this module's own `app.exit(0)` — which is
/// how the barrier avoids re-entering itself.
pub fn on_exit_requested(app: &AppHandle, code: Option<i32>) -> bool {
    if code.is_some() {
        return false;
    }
    // Claimed here, on the thread that receives the exit request: a second ⌘Q arriving while the
    // coordinator is starting must see the barrier already claimed, or it would escape it.
    let ticket = match app.state::<AppState>().quit.begin(BarrierReason::Quit) {
        Begin::Proceed => return false,
        Begin::Hold => return true,
        Begin::Start(ticket) => ticket,
    };
    let barrier_app = app.clone();
    let host = app.clone();
    if let Err(error) = crate::jobs::worker(&host, "quit-flush", move || {
        // Only an acknowledged save may exit; `app.exit(0)` re-enters `ExitRequested` with a code,
        // which the check above lets through.
        if coordinate(&barrier_app, ticket, BarrierReason::Quit).is_safe() {
            // `RunEvent::Exit` still does the sidecar flush + WAL checkpoint.
            barrier_app.exit(0);
        } else {
            tracing::warn!("quit cancelled: unsaved Develop edits are still held in the editor");
        }
        Ok(())
    }) {
        tracing::error!(%error, "quit coordinator failed to start");
        abandon(
            &host,
            BarrierReason::Quit,
            format!("quit coordinator: {error}"),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_names_who_was_asked_and_why_it_stopped() {
        let quit = report_for(BarrierReason::Quit, &BarrierResult::NoAcknowledgement);
        assert_eq!(quit.reason, "quit");
        assert!(quit.message.contains("quit"), "{}", quit.message);

        let updater = report_for(
            BarrierReason::UpdaterRestart,
            &BarrierResult::Failed("read-only volume".into()),
        );
        assert_eq!(updater.reason, "updater-restart");
        assert_eq!(updater.message, "read-only volume");

        // A flushed barrier has nothing to say; callers never emit for it.
        assert!(report_for(BarrierReason::Quit, &BarrierResult::Flushed)
            .message
            .is_empty());
    }

    #[test]
    fn the_flush_request_carries_the_barrier_id_camel_cased() {
        let json = serde_json::to_string(&FlushRequest { barrier_id: 7 }).unwrap();
        assert_eq!(json, r#"{"barrierId":7}"#);
        let saved: SavedRevisionArg =
            serde_json::from_str(r#"{"imageId":3,"revision":9}"#).unwrap();
        assert_eq!((saved.image_id, saved.revision), (3, 9));
    }
}
