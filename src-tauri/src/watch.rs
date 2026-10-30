//! Filesystem watcher: keeps the catalog in sync with on-disk changes under the watched roots.
//!
//! A `notify` recommended watcher feeds FS events into a coalescing thread that, after a short quiet
//! window, reconciles present/missing status and indexes any new RAW files, then emits
//! `library:changed` so the UI refreshes. Indexing is idempotent (known paths are skipped), so the
//! app's own import writes don't create duplicates.

use crate::state::AppState;
use core_library::THUMB_SIZE;
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Quiet period after the last event before a reconcile/index pass runs.
const DEBOUNCE: Duration = Duration::from_millis(800);

/// A watcher decode is valid only if no publication crossed its lifetime. The active count alone
/// cannot detect a job that has already finished. Begin/end and snapshot checks share the DB mutex,
/// so a writer also cannot start between the watcher's final check and its catalog commit.
#[derive(Default)]
pub(crate) struct PublicationFence {
    active: AtomicUsize,
    generation: AtomicU64,
    pending: AtomicBool,
}

impl PublicationFence {
    fn begin<T>(&self, lock: &Mutex<T>) {
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.active.fetch_add(1, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn end<T>(&self, lock: &Mutex<T>) {
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst) > 0
    }

    fn snapshot<T>(&self, _guard: &MutexGuard<'_, T>) -> Option<u64> {
        (!self.is_active()).then(|| self.generation.load(Ordering::SeqCst))
    }

    fn is_current<T>(&self, guard: &MutexGuard<'_, T>, generation: u64) -> bool {
        self.snapshot(guard) == Some(generation)
    }

    fn request_retry(&self) {
        self.pending.store(true, Ordering::SeqCst);
    }

    fn take_retry_if_idle(&self) -> bool {
        !self.is_active() && self.pending.swap(false, Ordering::SeqCst)
    }
}

fn schedule_pending<R: Runtime>(app: &AppHandle<R>) {
    if app.state::<AppState>().publication.take_retry_if_idle() {
        let app = app.clone();
        std::thread::spawn(move || sync(&app));
    }
}

fn request_resync<R: Runtime>(app: &AppHandle<R>) {
    app.state::<AppState>().publication.request_retry();
    // The last guard may have dropped before this request was recorded. In that case this caller
    // schedules the retry itself; otherwise the guard's drop schedules it when publication ends.
    schedule_pending(app);
}

/// Start watching the current watched roots. Returns the watcher, which the caller MUST keep alive
/// (store in `AppState`) — dropping it stops watching. `None` if there are no roots or setup fails.
pub fn spawn_watcher(app: AppHandle) -> Option<RecommendedWatcher> {
    let roots = watched_roots(&app);
    if roots.is_empty() {
        return None;
    }

    let (tx, rx) = channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            // Structural changes only — ignore pure metadata/access noise.
            let relevant = matches!(
                ev.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(ModifyKind::Name(_))
            );
            if relevant {
                let _ = tx.send(());
            }
        }
    })
    .ok()?;

    for root in &roots {
        let _ = watcher.watch(root, RecursiveMode::Recursive);
    }

    // Coalescing worker: wake on the first event, drain the quiet window, then sync once.
    let app2 = app.clone();
    std::thread::spawn(move || loop {
        if rx.recv().is_err() {
            break; // sender dropped — watcher gone
        }
        loop {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        sync(&app2);
    });

    Some(watcher)
}

/// Run a startup reconcile (catch changes made while the app was closed) and notify the UI.
pub fn reconcile_on_launch(app: &AppHandle) {
    let st = app.state::<AppState>();
    if let Ok(db) = st.db.lock() {
        let _ = core_library::reconcile(&db.conn);
    }
    let _ = app.emit("library:changed", ());
}

fn watched_roots<R: Runtime>(app: &AppHandle<R>) -> Vec<PathBuf> {
    let st = app.state::<AppState>();
    let Ok(db) = st.db.lock() else {
        return Vec::new();
    };
    core_library::list_folders(&db.conn)
        .map(|fs| fs.into_iter().map(|f| PathBuf::from(f.path)).collect())
        .unwrap_or_default()
}

/// Reconcile status + index new files under each root, then emit `library:changed` — but only when
/// something actually changed, so a no-op stat sweep doesn't trigger a full UI refresh.
///
/// Deferred while an import is in flight: the import catalogs its own files and drives its own UI
/// update (live-append + `import:done`), so running reconcile/index here would only race its
/// unlocked copy/process phase. The import's completion guard runs one sync afterward to catch any
/// real external change that landed during the import window (see `ImportGuard`).
pub(crate) fn sync<R: Runtime>(app: &AppHandle<R>) {
    let st = app.state::<AppState>();
    if st.publication.is_active() {
        request_resync(app);
        return;
    }

    let mut changed = false;
    if let Ok(db) = st.db.lock() {
        if st.publication.is_active() {
            request_resync(app);
            return;
        }
        if let Ok(rs) = core_library::reconcile(&db.conn) {
            changed |= rs.now_missing > 0 || rs.restored > 0;
        }
    }
    for root in watched_roots(app) {
        changed |= index_new(app, &root);
    }
    if changed {
        let _ = app.emit("library:changed", ());
    }
}

/// Index RAW files under `root` that aren't already in the catalog (idempotent). No progress events.
/// Returns `true` if at least one new row was inserted.
fn index_new<R: Runtime>(app: &AppHandle<R>, root: &Path) -> bool {
    let st = app.state::<AppState>();
    let (generation, folder_id, known, known_bad) = {
        let Ok(db) = st.db.lock() else { return false };
        let Some(generation) = st.publication.snapshot(&db) else {
            request_resync(app);
            return false;
        };
        let Ok(fid) = core_library::add_root(&db.conn, root) else {
            return false;
        };
        (
            generation,
            fid,
            core_library::existing_paths(&db.conn).unwrap_or_default(),
            core_library::skippable_failures(&db.conn).unwrap_or_default(),
        )
    };

    // Files whose recorded failure still stands are not re-opened: the watcher wakes on every card
    // mount and folder touch, so without this an undecodable file is re-decoded indefinitely.
    let todo: Vec<PathBuf> = core_library::enumerate_raws(root, true)
        .into_iter()
        .filter(|p| !known.contains(&p.display().to_string()))
        .filter(|p| !known_bad.contains(&p.display().to_string()))
        .collect();
    if todo.is_empty() {
        return false;
    }

    // The path rides along with its result: a `LibError` carries no path, and failures used to be
    // dropped here entirely (`results.iter().flatten()`), so a watcher-seen bad file went unrecorded.
    let results: Vec<_> = todo
        .par_iter()
        .map(|p| (p, core_library::process_file(p, &st.thumbs, THUMB_SIZE)))
        .collect();

    let imported_at = core_library::now_epoch();
    let mut inserted_any = false;
    let mut recorded = 0usize;
    if let Ok(mut db) = st.db.lock() {
        // Discard bytes decoded across any publication, even when the entire job has already
        // finished. Its replacement/cancellation may have changed or removed these paths.
        if !st.publication.is_current(&db, generation) {
            request_resync(app);
            return false;
        }
        if let Ok(tx) = db.conn.transaction() {
            for (path, r) in &results {
                match r {
                    Ok(p) => {
                        if let Ok(Some(_)) =
                            core_library::insert_image(&tx, folder_id, imported_at, p)
                        {
                            inserted_any = true;
                        }
                        let _ = core_library::clear_decode_failure(&tx, &p.path);
                    }
                    Err(e) => {
                        if core_library::record_decode_failure(
                            &tx,
                            Some(folder_id),
                            path,
                            e,
                            imported_at,
                        )
                        .is_ok()
                        {
                            recorded += 1;
                        }
                    }
                }
            }
            let _ = tx.commit();
        }
    }
    // A newly-recorded failure changes the sidebar's Unsupported count, so it counts as a change.
    inserted_any || recorded > 0
}

/// RAII marker that an import is running, so the FS watcher defers its sync pass for the duration
/// (preventing a double-index race once the import releases the DB lock between files). On drop —
/// the last in-flight import finishing — if the watcher tried to sync while gated, one sync is run
/// to reconcile any real on-disk change that happened during the import window.
pub struct ImportGuard<R: Runtime = tauri::Wry> {
    app: AppHandle<R>,
}

impl<R: Runtime> ImportGuard<R> {
    pub fn new(app: AppHandle<R>) -> Self {
        {
            let st = app.state::<AppState>();
            st.publication.begin(&st.db);
        }
        Self { app }
    }
}

impl<R: Runtime> Drop for ImportGuard<R> {
    fn drop(&mut self) {
        let st = self.app.state::<AppState>();
        st.publication.end(&st.db);
        schedule_pending(&self.app);
    }
}

#[cfg(test)]
mod tests {
    use super::PublicationFence;
    use std::sync::{mpsc, Arc, Mutex};

    #[test]
    fn publication_completed_during_decode_invalidates_snapshot_and_retries() {
        let fence = PublicationFence::default();
        let db = Mutex::new(());
        let generation = fence.snapshot(&db.lock().unwrap()).unwrap();
        // The watcher owns decoded bytes from before a whole import/export guard lifetime.
        fence.begin(&db);
        fence.end(&db);
        assert!(!fence.is_active());
        assert!(!fence.is_current(&db.lock().unwrap(), generation));
        // No guard remains to drain this late request, so the watcher must schedule it itself.
        fence.request_retry();
        assert!(fence.take_retry_if_idle());
        assert!(!fence.take_retry_if_idle());
    }

    #[test]
    fn overlapping_publications_defer_retry_until_the_last_finishes() {
        let fence = PublicationFence::default();
        let db = Mutex::new(());
        let generation = fence.snapshot(&db.lock().unwrap()).unwrap();
        fence.begin(&db);
        fence.begin(&db);
        fence.request_retry();
        fence.end(&db);
        assert!(fence.snapshot(&db.lock().unwrap()).is_none());
        assert!(!fence.take_retry_if_idle());
        fence.end(&db);
        assert!(!fence.is_current(&db.lock().unwrap(), generation));
        assert!(fence.take_retry_if_idle());
    }

    #[test]
    fn publication_start_waits_for_watcher_check_and_commit_lock() {
        let fence = Arc::new(PublicationFence::default());
        let db = Arc::new(Mutex::new(()));
        let insertion = db.lock().unwrap();
        let generation = fence.snapshot(&insertion).unwrap();
        let (attempt_tx, attempt_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let writer = {
            let fence = Arc::clone(&fence);
            let db = Arc::clone(&db);
            std::thread::spawn(move || {
                attempt_tx.send(()).unwrap();
                fence.begin(&db);
                started_tx.send(()).unwrap();
            })
        };
        attempt_rx.recv().unwrap();
        // The real begin helper shares the insertion mutex: it cannot cross this check/commit.
        assert!(started_rx.try_recv().is_err());
        assert!(fence.is_current(&insertion, generation));
        drop(insertion);
        started_rx.recv().unwrap();
        writer.join().unwrap();
        assert!(!fence.is_current(&db.lock().unwrap(), generation));
        fence.end(&db);
    }
}
