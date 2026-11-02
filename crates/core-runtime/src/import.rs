//! Native import service: lists a source folder, runs the background duplicate scan, commits an
//! import with progress and Stop, and applies the post-import marks.
//!
//! Import is long-running file IO, so it never runs on the ordered catalog lane. The service owns at
//! most one commit worker plus any number of (cancelled, soon-to-finish) listing workers, and
//! publishes an immutable [`ImportView`] the UI reads each frame. Catalog access goes through
//! [`CatalogAdapter::shared`]; every catalog commit of a run is fenced on the generation it started
//! on, so a closed or replaced catalog stops the run instead of receiving its rows.

use crate::catalog::{CatalogAdapter, SharedDb};
use core_app::import::{
    FileStatus, ImportFile, ImportMode, ImportOptions, ImportPhase, ImportPlan, ImportReport,
    ImportRequest, ImportView, Pairing,
};
use core_app::workflow::CullFlag;
use core_app::AppError;
use core_import::{ImportControl, NoSourceEvents, SourceEvents, SourceStatus};
use core_library::ThumbCache;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// Platform services the import dialog needs from the host.
pub trait ImportPlatform {
    /// Synchronous folder picker; `None` when the user cancels.
    fn pick_folder(&self, title: &str, start: Option<&Path>) -> Option<PathBuf>;
}

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inner {
    view: ImportView,
    finished: Option<ImportReport>,
    /// A commit ended (finished, failed or panicked) since the last `take_refresh`: rows may have
    /// been committed even when the run did not finish, so the host must re-read the library.
    refresh: bool,
}
type Shared = Arc<Mutex<Inner>>;

fn lock(shared: &Shared) -> MutexGuard<'_, Inner> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Run `f` on the shared state unless `cancel` is set. The check and the write share one lock, and
/// the superseding request sets `cancel` under the same lock, so a stale worker never publishes.
fn publish(shared: &Shared, cancel: &AtomicBool, f: impl FnOnce(&mut Inner)) -> bool {
    let mut guard = lock(shared);
    if cancel.load(Ordering::SeqCst) {
        return false;
    }
    f(&mut guard);
    true
}

struct Task {
    handle: JoinHandle<()>,
    /// Listing: superseded. Import: Stop requested.
    cancel: Arc<AtomicBool>,
}

pub struct ImportService {
    inner: Shared,
    wake: Wake,
    listings: Vec<Task>,
    import: Option<Task>,
    #[cfg(test)]
    start_gate: Option<Arc<AtomicBool>>,
}

impl ImportService {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        // Cards nest photos in DCIM subfolders: listing is recursive unless the user opts out
        // (legacy `import_list` default).
        let inner = Inner {
            view: ImportView {
                recursive: true,
                ..ImportView::default()
            },
            ..Inner::default()
        };
        Self {
            inner: Arc::new(Mutex::new(inner)),
            wake,
            listings: Vec::new(),
            import: None,
            #[cfg(test)]
            start_gate: None,
        }
    }

    /// Cheap snapshot: the file list is shared, not copied.
    pub fn view(&self) -> ImportView {
        lock(&self.inner).view.clone()
    }

    /// The report of a commit that finished since the last call (once), so the host can refresh the
    /// library.
    pub fn take_finished(&mut self) -> Option<ImportReport> {
        lock(&self.inner).finished.take()
    }

    /// Whether any commit ended since the last call (success or failure); the host refreshes the
    /// library on it, since a failed run may still have committed rows.
    pub fn take_refresh(&mut self) -> bool {
        std::mem::take(&mut lock(&self.inner).refresh)
    }

    pub fn handle(
        &mut self,
        request: ImportRequest,
        catalog: &Arc<CatalogAdapter>,
        catalog_path: &Path,
        platform: &dyn ImportPlatform,
    ) -> Result<(), AppError> {
        self.reap();
        let result = match request {
            ImportRequest::Open => self.open(catalog),
            ImportRequest::PickSource => self.pick_source(catalog, platform),
            ImportRequest::SetRecursive(recursive) => self.set_recursive(recursive, catalog),
            ImportRequest::PickDestination => self.pick_destination(platform),
            ImportRequest::Commit(plan) => self.commit(*plan, catalog, catalog_path),
            ImportRequest::Stop => {
                self.stop();
                Ok(())
            }
            ImportRequest::Close => {
                lock(&self.inner).view.open = false;
                Ok(())
            }
            ImportRequest::Dismiss => {
                self.dismiss();
                Ok(())
            }
        };
        (self.wake)();
        result
    }

    /// Request Stop on a running import and wait for it (the current chunk commits first). Listing
    /// workers are only cancelled: they are read-only and hold no catalog handle.
    pub fn stop_and_join(&mut self) {
        if let Some(task) = self.import.take() {
            task.cancel.store(true, Ordering::SeqCst);
            let _ = task.handle.join();
        }
        for task in self.listings.drain(..) {
            task.cancel.store(true, Ordering::SeqCst);
            if task.handle.is_finished() {
                let _ = task.handle.join();
            }
        }
    }

    fn reap(&mut self) {
        let (done, running): (Vec<_>, Vec<_>) = std::mem::take(&mut self.listings)
            .into_iter()
            .partition(|task| task.handle.is_finished());
        self.listings = running;
        for task in done {
            let _ = task.handle.join();
        }
        if self
            .import
            .as_ref()
            .is_some_and(|task| task.handle.is_finished())
        {
            if let Some(task) = self.import.take() {
                let _ = task.handle.join();
            }
        }
    }

    fn busy(&self) -> bool {
        lock(&self.inner).view.busy()
    }

    fn open(&mut self, catalog: &Arc<CatalogAdapter>) -> Result<(), AppError> {
        let (_, db) = catalog.shared()?;
        let root = load_library_root(&db)?;
        let mut guard = lock(&self.inner);
        guard.view.open = true;
        guard.view.library_root = root;
        Ok(())
    }

    fn pick_source(
        &mut self,
        catalog: &Arc<CatalogAdapter>,
        platform: &dyn ImportPlatform,
    ) -> Result<(), AppError> {
        self.refuse_if_busy()?;
        let start = {
            let guard = lock(&self.inner);
            guard
                .view
                .source
                .as_ref()
                .and_then(|source| source.parent().map(Path::to_path_buf))
                .or_else(|| guard.view.library_root.clone())
        };
        let Some(dir) = platform.pick_folder("Choose a folder to import", start.as_deref()) else {
            return Ok(());
        };
        lock(&self.inner).view.source = Some(dir);
        self.start_listing(catalog)
    }

    fn set_recursive(
        &mut self,
        recursive: bool,
        catalog: &Arc<CatalogAdapter>,
    ) -> Result<(), AppError> {
        self.refuse_if_busy()?;
        let has_source = {
            let mut guard = lock(&self.inner);
            guard.view.recursive = recursive;
            guard.view.source.is_some()
        };
        if has_source {
            self.start_listing(catalog)?;
        }
        Ok(())
    }

    fn pick_destination(&mut self, platform: &dyn ImportPlatform) -> Result<(), AppError> {
        self.refuse_if_busy()?;
        let start = {
            let guard = lock(&self.inner);
            guard
                .view
                .destination
                .clone()
                .or_else(|| guard.view.library_root.clone())
        };
        if let Some(dir) = platform.pick_folder("Choose the import destination", start.as_deref()) {
            lock(&self.inner).view.destination = Some(dir);
        }
        Ok(())
    }

    fn dismiss(&mut self) {
        let mut guard = lock(&self.inner);
        if matches!(
            guard.view.phase,
            ImportPhase::Finished(_) | ImportPhase::Failed(_)
        ) {
            guard.view.phase = if guard.view.source.is_some() {
                ImportPhase::Ready
            } else {
                ImportPhase::Idle
            };
        }
    }

    fn stop(&mut self) {
        let mut guard = lock(&self.inner);
        if let ImportPhase::Importing { done, total } = guard.view.phase {
            guard.view.phase = ImportPhase::Stopping { done, total };
        }
        if let Some(task) = &self.import {
            task.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn refuse_if_busy(&self) -> Result<(), AppError> {
        if self.busy() {
            Err(AppError::InvalidCommand("an import is running".into()))
        } else {
            Ok(())
        }
    }

    /// Supersede any earlier listing/scan and start one for the current source.
    fn start_listing(&mut self, catalog: &Arc<CatalogAdapter>) -> Result<(), AppError> {
        let (generation, db) = catalog.shared()?;
        let cancel = Arc::new(AtomicBool::new(false));
        let (source, recursive) = {
            let mut guard = lock(&self.inner);
            for task in &self.listings {
                task.cancel.store(true, Ordering::SeqCst);
            }
            guard.view.files = Arc::new(Vec::new());
            guard.view.dedup = (0, 0);
            guard.view.phase = ImportPhase::Listing;
            guard.view.generation += 1;
            (guard.view.source.clone(), guard.view.recursive)
        };
        let Some(source) = source else {
            return Ok(());
        };
        let job = ListJob {
            inner: Arc::clone(&self.inner),
            wake: Arc::clone(&self.wake),
            cancel: Arc::clone(&cancel),
            catalog: Arc::clone(catalog),
            generation,
            source,
            recursive,
        };
        let spawned = std::thread::Builder::new()
            .name("import-list".into())
            .spawn(move || job.run(&db));
        match spawned {
            Ok(handle) => {
                self.listings.push(Task { handle, cancel });
                Ok(())
            }
            Err(error) => {
                let message = format!("could not start the import listing: {error}");
                lock(&self.inner).view.phase = ImportPhase::Failed(message.clone());
                Err(AppError::Catalog(message.into()))
            }
        }
    }

    fn commit(
        &mut self,
        plan: ImportPlan,
        catalog: &Arc<CatalogAdapter>,
        catalog_path: &Path,
    ) -> Result<(), AppError> {
        let invalid = |message: &str| Err(AppError::InvalidCommand(message.into()));
        let destination = {
            let guard = lock(&self.inner);
            if guard.view.busy() {
                return invalid("an import is already running");
            }
            if plan.selected.is_empty() {
                return invalid("no files selected");
            }
            if guard.view.source.as_deref() != Some(plan.source.as_path()) {
                return invalid("the import source changed; review the selection");
            }
            let listed: HashSet<&str> = guard.view.files.iter().map(|f| f.path.as_str()).collect();
            if plan
                .selected
                .iter()
                .any(|path| !listed.contains(path.as_str()))
            {
                return invalid("a selected file is not in the current listing");
            }
            let destination = plan
                .destination
                .clone()
                .or_else(|| guard.view.destination.clone())
                .or_else(|| guard.view.library_root.clone());
            match (plan.mode, destination) {
                (ImportMode::Reference, destination) => {
                    destination.unwrap_or_else(|| plan.source.clone())
                }
                (_, Some(destination)) => destination,
                (_, None) => return invalid("choose a destination folder for Copy or Move"),
            }
        };
        let (generation, db) = catalog.shared()?;
        let thumbs = catalog_path
            .parent()
            .map(|dir| dir.join("thumbs"))
            .ok_or_else(|| AppError::Catalog("catalog path has no directory".to_string().into()))
            .and_then(|root| {
                ThumbCache::new(root).map_err(|e| {
                    AppError::Catalog(format!("could not open the thumbnail cache: {e}").into())
                })
            })?;
        let total = plan.selected.len();
        lock(&self.inner).view.phase = ImportPhase::Importing { done: 0, total };
        let stop = Arc::new(AtomicBool::new(false));
        let job = ImportJob {
            inner: Arc::clone(&self.inner),
            wake: Arc::clone(&self.wake),
            stop: Arc::clone(&stop),
            catalog: Arc::clone(catalog),
            generation,
            thumbs,
            destination,
            plan,
            #[cfg(test)]
            gate: self.start_gate.clone(),
        };
        let spawned = std::thread::Builder::new()
            .name("import-commit".into())
            .spawn(move || job.run(&db));
        match spawned {
            Ok(handle) => {
                self.import = Some(Task {
                    handle,
                    cancel: stop,
                });
                Ok(())
            }
            Err(error) => {
                let message = format!("could not start the import: {error}");
                lock(&self.inner).view.phase = ImportPhase::Failed(message.clone());
                Err(AppError::Catalog(message.into()))
            }
        }
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

fn db_error(error: impl std::fmt::Display) -> AppError {
    AppError::Catalog(error.to_string().into())
}

/// Configured library root, else the parent of the first watched folder (catalogs created before
/// the setting existed).
fn load_library_root(db: &SharedDb) -> Result<Option<PathBuf>, AppError> {
    let guard = db.lock().map_err(db_error)?;
    if let Some(root) = core_library::library_root(&guard.conn).map_err(db_error)? {
        return Ok(Some(PathBuf::from(root)));
    }
    let first: Option<String> = guard
        .conn
        .query_row("SELECT path FROM folders ORDER BY id LIMIT 1", [], |r| {
            r.get(0)
        })
        .ok();
    Ok(first.and_then(|path| Path::new(&path).parent().map(Path::to_path_buf)))
}

/// Present content hashes + sizes; the size set drives `dedup_scan`'s read prefilter.
fn present_identities(db: &SharedDb) -> Result<(HashSet<[u8; 32]>, HashSet<i64>), AppError> {
    let guard = db.lock().map_err(db_error)?;
    let mut stmt = guard
        .conn
        .prepare("SELECT content_hash, file_size FROM images WHERE status = 'present'")
        .map_err(db_error)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))
        .map_err(db_error)?;
    let (mut hashes, mut sizes) = (HashSet::new(), HashSet::new());
    for (hash, size) in rows.filter_map(Result::ok) {
        if let Ok(hash) = <[u8; 32]>::try_from(hash.as_slice()) {
            hashes.insert(hash);
        }
        sizes.insert(size);
    }
    Ok((hashes, sizes))
}

fn map_status(status: SourceStatus) -> FileStatus {
    match status {
        SourceStatus::Pending => FileStatus::Pending,
        SourceStatus::New => FileStatus::New,
        SourceStatus::DuplicateLibrary => FileStatus::DuplicateLibrary,
        SourceStatus::DuplicateBatch => FileStatus::DuplicateBatch,
    }
}

fn map_file(file: &core_import::SourceFile) -> ImportFile {
    ImportFile {
        path: file.path.clone(),
        filename: file.filename.clone(),
        size_bytes: file.size_bytes,
        mtime: file.mtime,
        status: map_status(file.status),
        kind: file.kind.clone(),
        pair_key: file.pair_key.clone(),
        pair_role: file.pair_role.clone(),
    }
}

struct ListJob {
    inner: Shared,
    wake: Wake,
    cancel: Arc<AtomicBool>,
    catalog: Arc<CatalogAdapter>,
    generation: u64,
    source: PathBuf,
    recursive: bool,
}

impl ListJob {
    fn run(self, db: &SharedDb) {
        let outcome = catch_unwind(AssertUnwindSafe(|| self.list_and_scan(db)));
        if let Err(payload) = outcome {
            let message = format!("import listing failed: {}", panic_text(&*payload));
            publish(&self.inner, &self.cancel, |i| {
                i.view.phase = ImportPhase::Failed(message);
            });
        }
        (self.wake)();
    }

    fn list_and_scan(&self, db: &SharedDb) {
        let listed = core_import::list_source(&self.source, self.recursive);
        let files: Vec<ImportFile> = listed.iter().map(map_file).collect();
        let paths: Vec<PathBuf> = listed.iter().map(|f| PathBuf::from(&f.path)).collect();
        let index: HashMap<String, usize> = files
            .iter()
            .enumerate()
            .map(|(k, f)| (f.path.clone(), k))
            .collect();
        let total = files.len();
        let shown = publish(&self.inner, &self.cancel, |i| {
            i.view.files = Arc::new(files);
            i.view.dedup = (0, total);
            i.view.phase = ImportPhase::Ready;
        });
        (self.wake)();
        if !shown || total == 0 {
            return;
        }
        let (hashes, sizes) = match present_identities(db) {
            Ok(identities) => identities,
            Err(error) => {
                publish(&self.inner, &self.cancel, |i| {
                    i.view.phase = ImportPhase::Failed(format!("duplicate check failed: {error}"));
                });
                return;
            }
        };
        core_import::dedup_scan(&paths, &hashes, &sizes, |done, total, batch| {
            if !self.catalog.is_current(self.generation) {
                return;
            }
            publish(&self.inner, &self.cancel, |i| {
                let files = Arc::make_mut(&mut i.view.files);
                for result in batch {
                    if let Some(&k) = index.get(&result.path) {
                        files[k].status = map_status(result.status);
                    }
                }
                i.view.dedup = (done, total);
            });
            (self.wake)();
        });
    }
}

/// Stop flag or a closed/replaced catalog stops the run between chunks; the same generation check
/// fences every catalog commit.
struct Control {
    stop: Arc<AtomicBool>,
    catalog: Arc<CatalogAdapter>,
    generation: u64,
}

impl ImportControl for Control {
    fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst) || !self.catalog.is_current(self.generation)
    }

    fn commit(&self, commit: &mut dyn FnMut(&mut dyn SourceEvents)) -> bool {
        if !self.catalog.is_current(self.generation) {
            return false;
        }
        commit(&mut NoSourceEvents);
        true
    }
}

struct ImportJob {
    inner: Shared,
    wake: Wake,
    stop: Arc<AtomicBool>,
    catalog: Arc<CatalogAdapter>,
    generation: u64,
    thumbs: ThumbCache,
    destination: PathBuf,
    plan: ImportPlan,
    #[cfg(test)]
    gate: Option<Arc<AtomicBool>>,
}

impl ImportJob {
    fn run(self, db: &SharedDb) {
        #[cfg(test)]
        if let Some(gate) = &self.gate {
            while !gate.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        let outcome = catch_unwind(AssertUnwindSafe(|| self.import(db)));
        let phase = match outcome {
            Ok(Ok(report)) => {
                lock(&self.inner).finished = Some(report.clone());
                ImportPhase::Finished(report)
            }
            Ok(Err(message)) => ImportPhase::Failed(message),
            Err(payload) => {
                ImportPhase::Failed(format!("import worker panicked: {}", panic_text(&*payload)))
            }
        };
        {
            let mut inner = lock(&self.inner);
            inner.view.phase = phase;
            inner.refresh = true;
        }
        (self.wake)();
    }

    fn import(&self, db: &SharedDb) -> Result<ImportReport, String> {
        let plan = &self.plan;
        let files: Vec<PathBuf> = plan.selected.iter().map(PathBuf::from).collect();
        let control = Control {
            stop: Arc::clone(&self.stop),
            catalog: Arc::clone(&self.catalog),
            generation: self.generation,
        };
        let added = RefCell::new(Vec::<i64>::new());
        let stats = core_import::import_files(
            db,
            &self.thumbs,
            &plan.source,
            &files,
            engine_mode(plan.mode),
            &self.destination,
            engine_pairing(plan.pairing),
            |done, total, row| {
                if let Some(row) = row {
                    added.borrow_mut().push(row.id);
                }
                let mut guard = lock(&self.inner);
                guard.view.phase = if self.stop.load(Ordering::SeqCst) {
                    ImportPhase::Stopping { done, total }
                } else {
                    ImportPhase::Importing { done, total }
                };
                drop(guard);
                (self.wake)();
            },
            &control,
        )
        .map_err(|e| e.to_string())?;
        let added = added.into_inner();
        // A stopped run keeps what it committed, and those rows get the marks the user asked for:
        // skipping them would silently import unmarked photos.
        let options_error = if added.is_empty() || !self.catalog.is_current(self.generation) {
            None
        } else {
            apply_options(db, &added, &plan.options).err()
        };
        Ok(ImportReport {
            session_id: stats.session_id,
            total: stats.total,
            added: stats.added,
            skipped: stats.skipped,
            failed: stats.failed,
            source_retained: stats.source_retained,
            paired: stats.paired,
            unsupported: stats.unsupported,
            not_processed: stats.not_processed,
            interrupted: stats.interrupted,
            options_error,
        })
    }
}

fn engine_mode(mode: ImportMode) -> core_import::ImportMode {
    match mode {
        ImportMode::Copy => core_import::ImportMode::Copy,
        ImportMode::Move => core_import::ImportMode::Move,
        ImportMode::Reference => core_import::ImportMode::Reference,
    }
}

fn engine_pairing(pairing: Pairing) -> core_import::Pairing {
    match pairing {
        Pairing::Pair => core_import::Pairing::Pair,
        Pairing::Standalone => core_import::Pairing::Standalone,
    }
}

/// Rating / flag / keywords / collection for the freshly added rows, under one brief catalog lock.
fn apply_options(db: &SharedDb, ids: &[i64], options: &ImportOptions) -> Result<(), String> {
    let mut guard = db.lock().map_err(|e| e.to_string())?;
    if let Some(stars) = options.rating {
        core_library::set_rating_many(&mut guard.conn, ids, i64::from(stars))
            .map_err(|e| e.to_string())?;
    }
    match options.flag {
        Some(flag @ (CullFlag::Pick | CullFlag::Reject)) => {
            core_library::set_flag_many(&mut guard.conn, ids, flag.as_str())
                .map_err(|e| e.to_string())?;
        }
        Some(CullFlag::None) | None => {}
    }
    for keyword in &options.keywords {
        let keyword = keyword.trim();
        if !keyword.is_empty() {
            core_library::add_keyword_to_images(&guard.conn, ids, keyword)
                .map_err(|e| e.to_string())?;
        }
    }
    let collection = match (options.collection_id, options.new_collection.as_deref()) {
        (Some(id), _) => Some(id),
        (None, Some(name)) if !name.trim().is_empty() => Some(
            core_library::create_collection(&guard.conn, name.trim(), false, None)
                .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };
    if let Some(collection) = collection {
        core_library::add_images_to_collection(&guard.conn, collection, ids)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    struct Picker(Mutex<VecDeque<PathBuf>>);
    impl Picker {
        fn new(dirs: &[&Path]) -> Self {
            Self(Mutex::new(dirs.iter().map(|d| d.to_path_buf()).collect()))
        }
    }
    impl ImportPlatform for Picker {
        fn pick_folder(&self, _: &str, _: Option<&Path>) -> Option<PathBuf> {
            self.0.lock().unwrap().pop_front()
        }
    }

    struct Env {
        _dir: tempfile::TempDir,
        catalog: Arc<CatalogAdapter>,
        catalog_path: PathBuf,
        service: ImportService,
        card: PathBuf,
    }

    fn fixture() -> Option<PathBuf> {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../library/2026/2026-06-06/_55A3947.CR3")
            .canonicalize()
            .ok()
    }

    fn env() -> Option<Env> {
        let raw = fixture()?;
        let dir = tempfile::tempdir().unwrap();
        let catalog_path = dir.path().join("data/catalog.db");
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        let catalog = Arc::new(CatalogAdapter::from_shared(
            crate::catalog::open_database(&catalog_path).unwrap(),
        ));
        let card = dir.path().join("card");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::copy(&raw, card.join("A.CR3")).unwrap();
        Some(Env {
            _dir: dir,
            catalog,
            catalog_path,
            service: ImportService::new(Arc::new(|| {})),
            card,
        })
    }

    impl Env {
        fn request(&mut self, request: ImportRequest, picker: &Picker) -> Result<(), AppError> {
            self.service
                .handle(request, &self.catalog, &self.catalog_path, picker)
        }

        fn wait(&self, what: &str, done: impl Fn(&ImportView) -> bool) -> ImportView {
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let view = self.service.view();
                if done(&view) {
                    return view;
                }
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn listed(&mut self) -> ImportView {
            let picker = Picker::new(&[&self.card.clone()]);
            self.request(ImportRequest::PickSource, &picker).unwrap();
            self.wait("dedup", |v| {
                v.phase == ImportPhase::Ready && v.dedup.0 == v.dedup.1 && !v.files.is_empty()
            })
        }

        fn plan(&self, view: &ImportView, mode: ImportMode, dest: Option<PathBuf>) -> ImportPlan {
            ImportPlan {
                source: view.source.clone().unwrap(),
                mode,
                destination: dest,
                selected: view.files.iter().map(|f| f.path.clone()).collect(),
                ..ImportPlan::default()
            }
        }

        fn image_count(&self) -> i64 {
            let (_, db) = self.catalog.shared().unwrap();
            let db = db.lock().unwrap();
            db.conn
                .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
                .unwrap()
        }

        fn finish(&self) -> ImportReport {
            let view = self.wait("finish", |v| matches!(v.phase, ImportPhase::Finished(_)));
            match view.phase {
                ImportPhase::Finished(report) => report,
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn reference_import_adds_one_and_reimport_is_idempotent() {
        let Some(mut env) = env() else { return };
        let none = Picker::new(&[]);
        let view = env.listed();
        let plan = env.plan(&view, ImportMode::Reference, None);
        env.request(ImportRequest::Commit(Box::new(plan.clone())), &none)
            .unwrap();
        let report = env.finish();
        assert_eq!((report.added, report.skipped, report.failed), (1, 0, 0));
        assert_eq!(env.image_count(), 1);
        assert_eq!(env.service.take_finished().map(|r| r.added), Some(1));
        assert!(env.service.take_finished().is_none());

        env.request(ImportRequest::Commit(Box::new(plan)), &none)
            .unwrap();
        let report = env.finish();
        assert_eq!((report.added, report.skipped), (0, 1));
        assert_eq!(env.image_count(), 1);
    }

    #[test]
    fn copy_lands_under_dated_folder() {
        let Some(mut env) = env() else { return };
        let none = Picker::new(&[]);
        let view = env.listed();
        let dest = env._dir.path().join("library");
        let plan = env.plan(&view, ImportMode::Copy, Some(dest.clone()));
        env.request(ImportRequest::Commit(Box::new(plan)), &none)
            .unwrap();
        assert_eq!(env.finish().added, 1);
        let found = walk(&dest)
            .into_iter()
            .map(|p| p.strip_prefix(&dest).unwrap().to_path_buf())
            .find(|p| {
                let parts: Vec<_> = p.iter().map(|s| s.to_string_lossy().into_owned()).collect();
                parts.len() == 3
                    && parts[0].len() == 4
                    && parts[1].len() == 10
                    && parts[1].starts_with(&parts[0])
            });
        assert!(found.is_some(), "no YYYY/YYYY-MM-DD file under {dest:?}");
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(walk(&path));
            } else {
                out.push(path);
            }
        }
        out
    }

    #[test]
    fn stop_before_start_is_interrupted() {
        let Some(mut env) = env() else { return };
        let none = Picker::new(&[]);
        let view = env.listed();
        let gate = Arc::new(AtomicBool::new(false));
        env.service.start_gate = Some(Arc::clone(&gate));
        let plan = env.plan(&view, ImportMode::Reference, None);
        env.request(ImportRequest::Commit(Box::new(plan)), &none)
            .unwrap();
        env.request(ImportRequest::Stop, &none).unwrap();
        assert!(matches!(
            env.service.view().phase,
            ImportPhase::Stopping { .. }
        ));
        gate.store(true, Ordering::SeqCst);
        let report = env.finish();
        assert!(report.interrupted);
        assert_eq!(report.added, 0);
        assert_eq!(env.image_count(), 0);
    }

    #[test]
    fn commit_with_unlisted_path_is_refused() {
        let Some(mut env) = env() else { return };
        let none = Picker::new(&[]);
        let view = env.listed();
        let mut plan = env.plan(&view, ImportMode::Reference, None);
        plan.selected = vec!["/definitely/not/listed.CR3".into()];
        let error = env
            .request(ImportRequest::Commit(Box::new(plan)), &none)
            .unwrap_err();
        assert!(matches!(error, AppError::InvalidCommand(_)), "{error:?}");
        let mut empty = env.plan(&view, ImportMode::Reference, None);
        empty.selected.clear();
        assert!(env
            .request(ImportRequest::Commit(Box::new(empty)), &none)
            .is_err());
        assert_eq!(env.service.view().phase, ImportPhase::Ready);
    }

    #[test]
    fn superseded_listing_is_not_published() {
        let Some(mut env) = env() else { return };
        let other = env._dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::copy(env.card.join("A.CR3"), other.join("B.CR3")).unwrap();
        let picker = Picker::new(&[&env.card.clone(), &other]);
        env.request(ImportRequest::PickSource, &picker).unwrap();
        let first = env.service.view().generation;
        env.request(ImportRequest::PickSource, &picker).unwrap();
        assert!(env.service.view().generation > first);
        let view = env.wait("second listing", |v| {
            v.phase == ImportPhase::Ready && v.dedup.0 == v.dedup.1 && !v.files.is_empty()
        });
        assert_eq!(view.source.as_deref(), Some(other.as_path()));
        assert!(view
            .files
            .iter()
            .all(|f| f.path.starts_with(other.to_str().unwrap())));
        // Give the cancelled first worker time to (not) publish, then re-check.
        std::thread::sleep(Duration::from_millis(500));
        let view = env.service.view();
        assert!(view.files.iter().all(|f| f.filename == "B.CR3"));
    }
}
