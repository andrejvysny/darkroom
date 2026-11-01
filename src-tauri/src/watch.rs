//! Filesystem watcher: keeps the catalog in sync with on-disk changes under the watched roots.
//!
//! A `notify` recommended watcher feeds FS events into a coalescing thread that, after a short quiet
//! window, reconciles present/missing status and indexes any new RAW files, then emits
//! `library:changed` so the UI refreshes. Indexing is idempotent (known paths are skipped), so the
//! app's own import writes don't create duplicates.

use crate::state::AppState;
use core_library::THUMB_SIZE;
use core_model::identity::ImageId;
use core_runtime::publication::{SourceKey, SourceMutationGuard};
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
            ("present", false) => changes.push((id, path, status, "missing".to_string())),
            ("missing", true) => changes.push((id, path, status, "present".to_string())),
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
    // A `present` transition whose identity cannot be read aborts the whole pass (see
    // `apply_status_updates`): the transaction drops uncommitted and the next watcher pass retries.
    let Some(applied) =
        apply_status_updates(&tx, &changes, &|| st.workers.is_stopping(), &|tx, id| {
            identity_of(tx, id)
        })
    else {
        return false;
    };
    if st.workers.is_stopping() {
        return false;
    }
    if tx.commit().is_ok() && !applied.is_empty() {
        drop(db);
        apply_status_changes(&mut gate, &applied);
        st.clear_source_caches();
        true
    } else {
        false
    }
}

/// Read a row's committed source identity inside the reconcile transaction. `None` when the row
/// is gone or the catalog could not answer — the caller must treat both as unreadable.
fn identity_of(tx: &core_db::rusqlite::Transaction<'_>, id: i64) -> Option<SourceKey> {
    core_library::image_by_id(tx, id)
        .ok()
        .flatten()
        .map(|row| SourceKey {
            hash: row.content_hash,
            path: row.path,
        })
}

/// Apply the candidate status transitions inside the reconcile transaction, returning the rows
/// actually updated together with their committed identities — or `None` when the caller must
/// drop the transaction (rollback) and retry on the next pass: shutdown was requested, or a row
/// becoming `present` had no readable identity. Retiring an id whose row is present is never
/// safe: the gate's `capture` refuses a `None` key forever, so the image would stay unpublishable
/// (sidecar mirroring, thumbnails, develop) for the whole run while catalog and disk both say
/// `present` and reconcile never re-fires. Aborting before the commit keeps the row `missing` so
/// the next pass re-attempts the transition.
///
/// `stopping` and the per-row identity read are seams so this runs in tests without an
/// `AppHandle`; production passes the worker-stop flag and [`identity_of`].
fn apply_status_updates(
    tx: &core_db::rusqlite::Transaction<'_>,
    changes: &[(i64, String, String, String)],
    stopping: &dyn Fn() -> bool,
    identity: &dyn Fn(&core_db::rusqlite::Transaction<'_>, i64) -> Option<SourceKey>,
) -> Option<Vec<AppliedStatusChange>> {
    let mut applied = Vec::new();
    for (id, path, old, new) in changes {
        if stopping() {
            return None;
        }
        if tx
            .execute(
                "UPDATE images SET status=?1 WHERE id=?2 AND path=?3 AND status=?4",
                core_db::rusqlite::params![new, id, path, old],
            )
            .unwrap_or(0)
            > 0
        {
            // Read the identity inside the transaction: if the commit later fails, these keys
            // never reached the catalog and the gate must not learn about them.
            let key = if new == "present" {
                Some(identity(tx, *id)?)
            } else {
                None
            };
            applied.push(AppliedStatusChange { id: *id, key });
        }
    }
    Some(applied)
}

/// A status row the reconcile transaction actually changed, together with the source identity
/// read from the catalog inside that same transaction.
struct AppliedStatusChange {
    id: i64,
    /// Transitions to `present` carry their committed path+hash (an unreadable identity aborts
    /// the pass before the commit, so it is never `None` here); transitions to `missing` carry
    /// `None` — the file is gone from disk and the id is retired.
    key: Option<SourceKey>,
}

/// Apply committed status transitions to the publication gate one source at a time. A single
/// file appearing/disappearing MUST NOT bump the catalog epoch: that supersedes every in-flight
/// job keyed on the epoch and stales pending publications for unrelated images. A `None` key
/// retires the id — the safe direction, since tokens for that image go stale and are re-captured.
fn apply_status_changes(gate: &mut SourceMutationGuard, applied: &[AppliedStatusChange]) {
    for change in applied {
        match &change.key {
            Some(key) => gate.replace_source(ImageId(change.id), key.clone()),
            None => gate.retire(ImageId(change.id)),
        }
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
        // Hold the source-mutation gate across the commit to serialize it against other source
        // mutations (lock order: gate before catalog). A fresh insert can REUSE a rowid retired by
        // a delete (`images.id` has no AUTOINCREMENT), so the committed row's identity is
        // registered under this same guard (see `commit_indexed`) — without it, work for the
        // reused id is refused as Stale until restart. A successful insert otherwise invalidates
        // nothing, and bumping the epoch here would wrongly supersede in-flight jobs for
        // unrelated images.
        let mut gate = st.publications.source_guard();
        let Ok(mut db) = st.db.lock() else { break };
        if st.workers.is_stopping() {
            break;
        }
        if !st.publication.is_current(&db, generation) {
            drop(db);
            request_resync(app);
            return inserted_any || recorded > 0;
        }
        let outcome = commit_indexed(
            &mut gate,
            &mut db.conn,
            folder_id,
            imported_at,
            path,
            &|| st.workers.is_stopping(),
            &result,
        );
        drop(db);
        match outcome {
            IndexCommit::Inserted => {
                inserted_any = true;
                st.clear_source_caches();
            }
            IndexCommit::FailureRecorded => recorded += 1,
            IndexCommit::Stopped => break,
            IndexCommit::Nothing | IndexCommit::Failed => {}
        }
    }
    // A newly-recorded failure changes the sidebar's Unsupported count, so it counts as a change.
    inserted_any || recorded > 0
}

/// Outcome of one fenced watcher catalog commit (see [`commit_indexed`]).
enum IndexCommit {
    /// A fresh row was inserted and committed; the gate now holds its identity.
    Inserted,
    /// A decode failure was recorded in the committed transaction.
    FailureRecorded,
    /// The transaction committed without writing a new identity (e.g. a duplicate hash).
    Nothing,
    /// Shutdown was requested before the commit; the transaction was rolled back.
    Stopped,
    /// The transaction could not be created or committed; nothing was written.
    Failed,
}

/// Commit one processed watcher file under the caller's HELD source guard: one transaction for
/// the row write, then — while the gate is still held — register the inserted row's identity
/// (hash + path) so a REUSED rowid (SQLite recycles ids after deletes; no AUTOINCREMENT) is
/// publishable immediately instead of being refused as Stale until restart. Registering inside
/// the guard, not after releasing it, keeps a concurrent retire/reset from slipping between the
/// commit and the registration. Lock order: the caller holds the gate; the catalog lock is
/// taken through `conn` inside (gate → catalog).
fn commit_indexed(
    gate: &mut SourceMutationGuard<'_>,
    conn: &mut core_db::rusqlite::Connection,
    folder_id: i64,
    imported_at: i64,
    src_path: &Path,
    stopping: &dyn Fn() -> bool,
    result: &Result<core_library::ProcessedImage, core_library::LibError>,
) -> IndexCommit {
    let Ok(tx) = conn.transaction() else {
        return IndexCommit::Failed;
    };
    let mut inserted: Option<i64> = None;
    let mut recorded_failure = false;
    match result {
        Ok(p) => {
            // As before: a failed insert is not a watcher error — no row appears, and the
            // transaction still commits the failure-record cleanup.
            inserted = core_library::insert_image(&tx, folder_id, imported_at, p).unwrap_or(None);
            let _ = core_library::clear_decode_failure(&tx, &p.path);
        }
        Err(e) => {
            recorded_failure =
                core_library::record_decode_failure(&tx, Some(folder_id), src_path, e, imported_at)
                    .is_ok();
        }
    }
    if stopping() {
        return IndexCommit::Stopped; // tx drops uncommitted → rollback
    }
    if tx.commit().is_err() {
        return IndexCommit::Failed;
    }
    if let Some(id) = inserted {
        let p = result.as_ref().expect("insert only succeeds for Ok");
        gate.replace_source(
            ImageId(id),
            SourceKey {
                hash: p.content_hash_hex.clone(),
                path: p.path.clone(),
            },
        );
        return IndexCommit::Inserted;
    }
    if recorded_failure {
        IndexCommit::FailureRecorded
    } else {
        IndexCommit::Nothing
    }
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
