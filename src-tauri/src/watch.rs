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
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

/// Quiet period after the last event before a reconcile/index pass runs.
const DEBOUNCE: Duration = Duration::from_millis(800);

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

fn watched_roots(app: &AppHandle) -> Vec<PathBuf> {
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
pub(crate) fn sync(app: &AppHandle) {
    let st = app.state::<AppState>();
    if st.import_active.load(Ordering::SeqCst) > 0 {
        st.watch_pending.store(true, Ordering::SeqCst);
        return;
    }

    let mut changed = false;
    if let Ok(db) = st.db.lock() {
        if let Ok(rs) = core_library::reconcile(&db.conn) {
            changed |= rs.now_missing > 0 || rs.restored > 0;
        }
    }
    for root in watched_roots(app) {
        changed |= index_new(&st, &root);
    }
    if changed {
        let _ = app.emit("library:changed", ());
    }
}

/// Index RAW files under `root` that aren't already in the catalog (idempotent). No progress events.
/// Returns `true` if at least one new row was inserted.
fn index_new(st: &AppState, root: &Path) -> bool {
    let (folder_id, known) = {
        let Ok(db) = st.db.lock() else { return false };
        let Ok(fid) = core_library::add_root(&db.conn, root) else {
            return false;
        };
        (
            fid,
            core_library::existing_paths(&db.conn).unwrap_or_default(),
        )
    };

    let todo: Vec<PathBuf> = core_library::enumerate_raws(root, true)
        .into_iter()
        .filter(|p| !known.contains(&p.display().to_string()))
        .collect();
    if todo.is_empty() {
        return false;
    }

    let results: Vec<_> = todo
        .par_iter()
        .map(|p| core_library::process_file(p, &st.thumbs, THUMB_SIZE))
        .collect();

    let imported_at = core_library::now_epoch();
    let mut inserted_any = false;
    if let Ok(mut db) = st.db.lock() {
        if let Ok(tx) = db.conn.transaction() {
            for r in results.iter().flatten() {
                if let Ok(Some(_)) = core_library::insert_image(&tx, folder_id, imported_at, r) {
                    inserted_any = true;
                }
            }
            let _ = tx.commit();
        }
    }
    inserted_any
}

/// RAII marker that an import is running, so the FS watcher defers its sync pass for the duration
/// (preventing a double-index race once the import releases the DB lock between files). On drop —
/// the last in-flight import finishing — if the watcher tried to sync while gated, one sync is run
/// to reconcile any real on-disk change that happened during the import window.
pub struct ImportGuard {
    app: AppHandle,
}

impl ImportGuard {
    pub fn new(app: AppHandle) -> Self {
        app.state::<AppState>()
            .import_active
            .fetch_add(1, Ordering::SeqCst);
        Self { app }
    }
}

impl Drop for ImportGuard {
    fn drop(&mut self) {
        let st = self.app.state::<AppState>();
        // `fetch_sub` returns the prior value; `== 1` means this was the last active import.
        if st.import_active.fetch_sub(1, Ordering::SeqCst) == 1
            && st.watch_pending.swap(false, Ordering::SeqCst)
        {
            let app = self.app.clone();
            std::thread::spawn(move || sync(&app));
        }
    }
}
