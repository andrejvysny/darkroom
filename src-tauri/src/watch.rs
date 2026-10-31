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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender};
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
    wake: Mutex<Option<SyncSender<()>>>,
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
    let fence = &app.state::<AppState>().publication;
    if !fence.is_active() && fence.pending.load(Ordering::SeqCst) {
        if let Ok(wake) = fence.wake.lock() {
            if let Some(wake) = wake.as_ref() {
                let _ = wake.try_send(());
            }
        }
    }
}

fn request_resync<R: Runtime>(app: &AppHandle<R>) {
    app.state::<AppState>().publication.request_retry();
    schedule_pending(app);
}

/// Start watching the current watched roots. Returns the watcher, which the caller MUST keep alive
/// (store in `AppState`) — dropping it stops watching. `None` if there are no roots or setup fails.
pub fn spawn_watcher(app: AppHandle) -> Option<RecommendedWatcher> {
    let roots = watched_roots(&app);

    let (tx, rx) = sync_channel::<()>(1);
    *app.state::<AppState>()
        .publication
        .wake
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());
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
                let _ = tx.try_send(());
            }
        }
    })
    .ok()?;

    for root in &roots {
        let _ = watcher.watch(root, RecursiveMode::Recursive);
    }

    // Coalescing worker: wake on the first event, drain the quiet window, then sync once.
    let app2 = app.clone();
    let worker = app.clone();
    if let Err(error) = crate::jobs::worker(&worker, "catalog-watcher", move || {
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => {
                    if app2.state::<AppState>().workers.is_stopping() {
                        break;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
            loop {
                match rx.recv_timeout(DEBOUNCE) {
                    Ok(()) if app2.state::<AppState>().workers.is_stopping() => return Ok(()),
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
            let st = app2.state::<AppState>();
            if st.workers.is_stopping() {
                break;
            }
            st.publication.take_retry_if_idle();
            loop {
                let work = app2.clone();
                match st.jobs.submit(
                    crate::jobs::spec(st.inner(), core_app::jobs::JobClass::Maintenance, false),
                    move |ctx| {
                        ctx.checkpoint()?;
                        sync(&work);
                        Ok(())
                    },
                ) {
                    Ok(job) => {
                        if matches!(
                            job.wait(),
                            Err(core_app::jobs::JobFailure::Cancelled
                                | core_app::jobs::JobFailure::Superseded)
                        ) && !st.workers.is_stopping()
                        {
                            continue;
                        }
                        break;
                    }
                    Err(core_app::jobs::SubmitError::Capacity) => {
                        if !st.jobs.wait_capacity(64 * 1024) {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(?error, "watcher request rejected");
                        break;
                    }
                }
            }
        }
        Ok(())
    }) {
        tracing::error!(%error, "watcher worker failed to start");
        return None;
    }

    schedule_pending(&app);
    Some(watcher)
}

/// Run a startup reconcile (catch changes made while the app was closed) and notify the UI.
pub fn reconcile_on_launch(app: &AppHandle) {
    reconcile_status(app);
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

    let mut changed = reconcile_status(app);
    for root in watched_roots(app) {
        if st.workers.is_stopping() {
            break;
        }
        changed |= index_new(app, &root);
    }
    if changed {
        let _ = app.emit("library:changed", ());
    }
}

fn reconcile_status<R: Runtime>(app: &AppHandle<R>) -> bool {
    let st = app.state::<AppState>();
    let (generation, rows): (u64, Vec<(i64, String, String)>) = {
        let Ok(db) = st.db.lock() else { return false };
        let Some(generation) = st.publication.snapshot(&db) else {
            drop(db);
            request_resync(app);
            return false;
        };
        let Ok(mut stmt) = db.conn.prepare("SELECT id, path, status FROM images") else {
            return false;
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))) else {
            return false;
        };
        let mut snapshot = Vec::new();
        for row in rows {
            if st.workers.is_stopping() {
                return false;
            }
            if let Ok(row) = row {
                snapshot.push(row);
            }
        }
        (generation, snapshot)
    };
    let mut changes = Vec::new();
    for (id, path, status) in rows {
        if st.workers.is_stopping() {
            return false;
        }
        let exists = Path::new(&path).exists();
        match (status.as_str(), exists) {
            ("present", false) => changes.push((id, path, status, "missing")),
            ("missing", true) => changes.push((id, path, status, "present")),
            _ => {}
        }
    }
    if changes.is_empty() {
        return false;
    }
    if st.workers.is_stopping() {
        return false;
    }
    let mut gate = st.publications.source_guard();
    let Ok(db) = st.db.lock() else { return false };
    if st.workers.is_stopping() {
        return false;
    }
    if !st.publication.is_current(&db, generation) {
        drop(db);
        drop(gate);
        request_resync(app);
        return false;
    }
    let Ok(tx) = db.conn.unchecked_transaction() else {
        return false;
    };
    let mut changed = false;
    for (id, path, old, new) in changes {
        if st.workers.is_stopping() {
            return false;
        }
        if tx
            .execute(
                "UPDATE images SET status=?1 WHERE id=?2 AND path=?3 AND status=?4",
                core_db::rusqlite::params![new, id, path, old],
            )
            .unwrap_or(0)
            > 0
        {
            changed = true;
        }
    }
    if st.workers.is_stopping() {
        return false;
    }
    if tx.commit().is_ok() && changed {
        drop(db);
        gate.invalidate_all();
        st.clear_source_caches();
        true
    } else {
        false
    }
}

fn supported_path(path: &Path) -> bool {
    let visible = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| !name.starts_with('.'));
    visible
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                core_library::SUPPORTED_EXT
                    .iter()
                    .any(|supported| extension.eq_ignore_ascii_case(supported))
            })
}

/// Index RAW files under `root` that aren't already in the catalog (idempotent). No progress events.
/// Returns `true` if at least one new row was inserted.
fn index_new<R: Runtime>(app: &AppHandle<R>, root: &Path) -> bool {
    let st = app.state::<AppState>();
    let (generation, folder_id, known, known_bad) = {
        let Ok(db) = st.db.lock() else { return false };
        let Some(generation) = st.publication.snapshot(&db) else {
            drop(db);
            request_resync(app);
            return false;
        };
        let Ok(fid) = db.conn.query_row(
            "SELECT id FROM folders WHERE path=?1",
            [root.to_string_lossy().as_ref()],
            |row| row.get::<_, i64>(0),
        ) else {
            return false;
        };
        (
            generation,
            fid,
            core_library::existing_paths(&db.conn).unwrap_or_default(),
            core_library::skippable_failures(&db.conn).unwrap_or_default(),
        )
    };

    let imported_at = core_library::now_epoch();
    let mut inserted_any = false;
    let mut recorded = 0usize;
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        if st.workers.is_stopping() {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() || !supported_path(entry.path()) {
            continue;
        }
        let path = entry.path();
        let key = path.display().to_string();
        if known.contains(&key) || known_bad.contains(&key) {
            continue;
        }
        if st.workers.is_stopping() {
            break;
        }
        let result = core_library::process_file(path, &st.thumbs, THUMB_SIZE);
        if st.workers.is_stopping() {
            break;
        }
        let mut gate = st.publications.source_guard();
        let Ok(mut db) = st.db.lock() else { break };
        if st.workers.is_stopping() {
            break;
        }
        if !st.publication.is_current(&db, generation) {
            drop(db);
            drop(gate);
            request_resync(app);
            return inserted_any || recorded > 0;
        }
        let mut published = false;
        if let Ok(tx) = db.conn.transaction() {
            let mut inserted = false;
            match &result {
                Ok(p) => {
                    if let Ok(Some(_)) = core_library::insert_image(&tx, folder_id, imported_at, p)
                    {
                        inserted = true;
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
            if st.workers.is_stopping() {
                break;
            }
            if tx.commit().is_ok() && inserted {
                gate.invalidate_all();
                published = true;
                inserted_any = true;
            }
        };
        drop(db);
        if published {
            st.clear_source_caches();
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
mod tests;
