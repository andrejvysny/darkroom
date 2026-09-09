//! Save barrier for a normal quit.
//!
//! Develop edits are debounced in the webview before they reach `develop_set_edit`, so at the
//! moment the user presses ⌘Q the newest edit can still be sitting in a JavaScript timer. Flushing
//! the WAL on exit cannot recover that — the row was never written. So the exit is held, the
//! frontend is asked to flush, and the process leaves only once it has acknowledged (or the wait
//! times out).
//!
//! Deliberately bounded in both directions:
//! - the ack wait is `ACK_TIMEOUT`, so an unresponsive or already-torn-down webview delays quit by
//!   at most that long instead of trapping the app;
//! - a flush that reports failure cancels the quit ONCE and leaves the error banner up; a second
//!   attempt within `RETRY_GRACE` exits regardless, so a failing disk can never hold the user
//!   hostage in their own editor.

use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// How long to wait for the frontend's `develop_flush_ack` before quitting anyway.
const ACK_TIMEOUT: Duration = Duration::from_millis(1500);
/// After a failed flush, a second quit attempt inside this window is honoured unconditionally.
const RETRY_GRACE: Duration = Duration::from_secs(30);

/// Event the frontend listens for; it replies with the `develop_flush_ack` command.
const FLUSH_EVENT: &str = "app:flush-edits";

#[derive(Default)]
struct QuitState {
    /// A barrier is already running — further exit requests must not start a second one.
    running: bool,
    /// Where `develop_flush_ack` delivers the frontend's answer.
    ack: Option<SyncSender<bool>>,
    /// When the last flush reported failure (see `RETRY_GRACE`).
    last_failure: Option<Instant>,
}

/// What an exit request should do.
enum Begin {
    /// Let the process exit now.
    Proceed,
    /// Prevent the exit; a barrier already running will finish the job.
    Hold,
    /// Prevent the exit and wait for the frontend on this receiver.
    Start(Receiver<bool>),
}

#[derive(Default)]
pub struct QuitBarrier {
    state: Mutex<QuitState>,
}

impl QuitBarrier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scheduling state only — recover from poisoning rather than killing the quit path.
    fn lock(&self) -> std::sync::MutexGuard<'_, QuitState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Deliver the frontend's answer. Ignored when no barrier is waiting (a late or stray ack).
    pub fn ack(&self, ok: bool) {
        let tx = self.lock().ack.take();
        if let Some(tx) = tx {
            let _ = tx.try_send(ok);
        }
    }

    /// Claim the barrier for this exit request.
    fn begin(&self) -> Begin {
        let mut st = self.lock();
        if st.running {
            // A barrier is already asking the frontend to flush. Hold this request too and let that
            // one finish: it either exits for us or cancels, and a ⌘Q landing inside its 1.5 s
            // window must not be the one that escapes with an edit still in memory.
            return Begin::Hold;
        }
        if st.last_failure.is_some_and(|t| t.elapsed() < RETRY_GRACE) {
            // The user was told the save failed and asked to quit again — honour it.
            st.last_failure = None;
            return Begin::Proceed;
        }
        let (tx, rx) = sync_channel(1);
        st.running = true;
        st.ack = Some(tx);
        Begin::Start(rx)
    }

    fn finish_failed(&self) {
        let mut st = self.lock();
        st.running = false;
        st.ack = None;
        st.last_failure = Some(Instant::now());
    }
}

/// Handle `RunEvent::ExitRequested`. Returns `true` when the caller must call `api.prevent_exit()`.
///
/// `code` is `Some` for a programmatic exit — including this module's own `app.exit(0)` — which is
/// how the barrier avoids re-entering itself.
pub fn on_exit_requested(app: &AppHandle, code: Option<i32>) -> bool {
    if code.is_some() {
        return false;
    }
    let barrier = &app.state::<AppState>().quit;
    let rx = match barrier.begin() {
        Begin::Proceed => return false,
        Begin::Hold => return true,
        Begin::Start(rx) => rx,
    };
    if app.emit(FLUSH_EVENT, ()).is_err() {
        // No webview to ask (already torn down) — nothing is pending that we could save.
        app.state::<AppState>().quit.ack(true);
    }
    let app = app.clone();
    // Off the main thread: the webview needs the run loop to deliver the ack back to us.
    std::thread::spawn(move || {
        let ok = match rx.recv_timeout(ACK_TIMEOUT) {
            Ok(ok) => ok,
            Err(_) => {
                tracing::warn!(
                    timeout_ms = ACK_TIMEOUT.as_millis() as u64,
                    "no flush acknowledgement before quit; exiting anyway"
                );
                true
            }
        };
        if ok {
            // `RunEvent::Exit` still does the sidecar flush + WAL checkpoint.
            app.exit(0);
        } else {
            tracing::warn!("quit cancelled: Develop edits could not be saved");
            app.state::<AppState>().quit.finish_failed();
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_request_is_held_while_a_barrier_runs() {
        let b = QuitBarrier::new();
        assert!(matches!(b.begin(), Begin::Start(_)));
        // A ⌘Q inside the flush window must be held, not allowed to escape with an unsaved edit.
        assert!(matches!(b.begin(), Begin::Hold));
    }

    #[test]
    fn the_ack_reaches_the_waiter() {
        let b = QuitBarrier::new();
        let Begin::Start(rx) = b.begin() else {
            panic!("first request must start the barrier");
        };
        b.ack(true);
        assert_eq!(rx.recv_timeout(Duration::from_millis(50)), Ok(true));
        // The channel is consumed: a stray second ack is a no-op, not a panic.
        b.ack(false);
    }

    #[test]
    fn quitting_again_after_a_failure_is_honoured_once() {
        let b = QuitBarrier::new();
        assert!(matches!(b.begin(), Begin::Start(_)));
        b.finish_failed();
        // The user has seen the banner and asked to quit anyway.
        assert!(matches!(b.begin(), Begin::Proceed));
        // …and the grace is spent, so a later quit gets the barrier again.
        assert!(matches!(b.begin(), Begin::Start(_)));
    }
}
