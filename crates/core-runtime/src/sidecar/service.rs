//! The background sidecar mirror for a host whose catalog can be opened, closed and replaced.
//!
//! The legacy Tauri host owns one connection for the process lifetime, so it can hold a single
//! [`OutboxHandle`] in `AppState` for as long as the app lives. A host that closes and reopens
//! catalogs (the native host, and any host using [`CatalogAdapter::open`]) cannot: a handle built
//! at startup would pin the connection it was built for, and after a reopen it would drain a
//! catalog nobody is looking at any more. This service owns the thread and re-binds a fresh
//! handle to whatever generation is open, so the durable queue is always drained against the
//! catalog that is actually current.
//!
//! Properties:
//! - **Never blocks a quit.** [`MirrorService::shutdown`] sets the stop flag, wakes the drain and
//!   joins; the drain checks the flag before every item, so the wait is bounded by one in-hand
//!   item plus the worker's own poll interval.
//! - **Never stranded without a catalog.** With no catalog open the thread parks for at most
//!   [`IDLE_POLL`], or until a save notifies it or the host shuts it down.
//! - **Never silent.** The loop body runs under `catch_unwind`; a panic is logged and ends the
//!   thread, and `shutdown` still joins it.

use super::outbox::OutboxHandle;
use crate::catalog::{CatalogAdapter, SharedDb};
use crate::import::panic_text;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// How long the thread may sleep when there is nothing to wake it. Same order as the drain's own
/// poll: the only thing it bounds is how fast a stop signal or a reopen is noticed, and a wake
/// that finds nothing to do costs one catalog read.
const IDLE_POLL: Duration = Duration::from_secs(1);

/// State shared between the host (which notifies and shuts down) and the mirror thread.
struct Shared {
    /// Set once by [`MirrorService::shutdown`]; checked before every item and every park.
    stopping: AtomicBool,
    /// The lock the no-catalog park waits on; guards nothing but the wait.
    park: Mutex<()>,
    wake: Condvar,
    /// The handle for the currently open generation, so `notify` reaches the live drain instead of
    /// a handle to a catalog that has since been closed.
    current: Mutex<Option<OutboxHandle>>,
    /// Raised before `on_progress` runs; lets a host poll instead of (or besides) being called.
    progressed: AtomicBool,
    on_progress: Arc<dyn Fn() + Send + Sync>,
}

impl Shared {
    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Park until notified or `timeout` elapses, whichever comes first.
    fn park(&self, timeout: Duration) {
        let guard = self.park.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            self.wake
                .wait_timeout(guard, timeout)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    fn set_current(&self, handle: Option<OutboxHandle>) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = handle;
    }

    fn progress(&self) {
        self.progressed.store(true, Ordering::SeqCst);
        (self.on_progress)();
    }

    fn current(&self) -> Option<OutboxHandle> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Background sidecar mirror for a host whose catalog can be opened, closed and replaced.
pub struct MirrorService {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl MirrorService {
    /// Start the mirror thread for `catalog`. The catalog need not be open yet: the thread parks
    /// until one is, and follows every later open/close/replacement on its own.
    /// `on_progress` is called from the mirror thread after a drain pass that mirrored, dropped or
    /// failed an intent.
    pub fn start(
        catalog: Arc<CatalogAdapter>,
        on_progress: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            stopping: AtomicBool::new(false),
            park: Mutex::new(()),
            wake: Condvar::new(),
            current: Mutex::new(None),
            progressed: AtomicBool::new(false),
            on_progress,
        });
        let thread_shared = Arc::clone(&shared);
        match std::thread::Builder::new()
            .name("sidecar-mirror".into())
            .spawn(move || loop_body(&thread_shared, &catalog))
        {
            Ok(thread) => Ok(Self {
                shared,
                thread: Some(thread),
            }),
            Err(error) => Err(format!("could not start the sidecar mirror: {error}")),
        }
    }

    /// Whether the mirror progressed since the last call; clears the flag.
    pub fn take_progress(&self) -> bool {
        self.shared.progressed.swap(false, Ordering::SeqCst)
    }

    /// A save/cull committed: wake the drain now (cheap, non-blocking).
    pub fn notify(&self) {
        // Both wakes are needed: the drain's own condvar (a due intent while a catalog is open) and
        // ours (the thread is parked because no catalog was open, or is between generations).
        if let Some(handle) = self.shared.current() {
            handle.notify();
        }
        self.shared.wake.notify_all();
    }

    /// Stop and join. Idempotent. Never blocks longer than one in-hand item + POLL.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.shared.stopping.store(true, Ordering::SeqCst);
        self.shared.wake.notify_all();
        if let Some(handle) = self.shared.current() {
            handle.shutdown();
        }
        match self.thread.take() {
            // The loop body catches its own panics, so a join failure means the thread died
            // outside it (a panic while logging) and the host should know the mirror is gone.
            Some(thread) => thread
                .join()
                .map_err(|_| "the sidecar mirror thread died outside its own handler".to_string()),
            None => Ok(()),
        }
    }
}

impl Drop for MirrorService {
    fn drop(&mut self) {
        // A dropped service must not leak its thread; a failed join at drop time has no one left
        // to tell, and nothing durable depends on it (the intents stay in the catalog).
        let _ = self.shutdown();
    }
}

/// The mirror thread body: follow the open catalog until stopped, and never let a panic escape
/// silently — log it and end the thread (the host's `shutdown` then still joins).
fn loop_body(shared: &Shared, catalog: &CatalogAdapter) {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        while !shared.stopping() {
            match catalog.shared() {
                Ok((generation, db)) => drain_generation(shared, catalog, generation, db),
                // No catalog open (never opened, or closed): park rather than spin.
                Err(_) => shared.park(IDLE_POLL),
            }
        }
    }));
    shared.set_current(None);
    if let Err(payload) = outcome {
        tracing::error!(
            panic = %panic_text(&*payload),
            "sidecar mirror thread stopped after a panic"
        );
    }
}

/// Drain one generation to completion: bind a handle to `db`, publish the current generation's
/// drain under the catalog's own fence, and unbind it when that catalog stops being current.
fn drain_generation(shared: &Shared, catalog: &CatalogAdapter, generation: u64, db: SharedDb) {
    let handle = OutboxHandle::new(db);
    shared.set_current(Some(handle.clone()));
    let publications = catalog.publications();
    handle.run(
        &publications,
        &|| shared.stopping() || !catalog.is_current(generation),
        &|| shared.progress(),
    );
    // `run` returns on its own stop flag; make that ordered for the next handle as well.
    handle.shutdown();
    shared.set_current(None);
}

#[cfg(test)]
mod tests;
