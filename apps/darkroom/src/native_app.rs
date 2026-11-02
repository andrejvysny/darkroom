//! The native host's application object: an [`AsyncCatalogWorkflow`] wired with the host's
//! clock, event sink and executor. Kept free of window/GPU dependencies (and of `crate::`
//! paths) so the headless `tests/edit_flow.rs` can include this file verbatim and drive it with
//! an injected clock.

use core_app::import::{ImportReport, ImportRequest, ImportView};
use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::state::{ApplicationSnapshot, MirrorStatus};
use core_app::subscription::{Notice, SnapshotUpdate};
use core_app::workflow::Command;
use core_app::AppError;
use core_runtime::async_workflow::AsyncCatalogWorkflow;
use core_runtime::import::{ImportPlatform, ImportService};
use core_runtime::sidecar::MirrorService;
use core_runtime::workflow::{InlineExecutor, RuntimePaths};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Wall-clock milliseconds, the same axis as `settings::now_ms`.
struct SystemClock;
impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}
struct NativeEvents;
impl EventSink for NativeEvents {
    fn published(&self, update: &SnapshotUpdate) {
        if let Notice::EffectCompleted {
            result: Err(error), ..
        } = &update.notice
        {
            eprintln!("catalog operation failed: {error}");
        }
    }
}
struct RenderCapability(bool);
impl RenderBackend for RenderCapability {
    fn available(&self) -> bool {
        self.0
    }
}

pub struct NativeApplication {
    workflow: AsyncCatalogWorkflow,
    catalog_path: PathBuf,
    clock: Arc<dyn Clock>,
    import: ImportService,
    /// Drains the durable sidecar outbox for the catalog the workflow owns. `None` once quit has
    /// joined it, or when it could never start (see [`Self::mirror_error`]).
    mirror: Option<MirrorService>,
    mirror_error: Option<String>,
}
impl NativeApplication {
    pub fn new(
        path: PathBuf,
        renderer_available: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, AppError> {
        Self::with_clock(path, renderer_available, wake, Arc::new(SystemClock))
    }
    /// Same as [`Self::new`] with an injected workflow clock (autosave debounce, backoff and
    /// the barrier timeout all run on it).
    #[allow(dead_code)] // the window host uses `new`; headless tests inject a clock
    pub fn with_clock(
        path: PathBuf,
        renderer_available: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, AppError> {
        let import = ImportService::new(Arc::clone(&wake));
        let mirror_wake = Arc::clone(&wake);
        let mut workflow = AsyncCatalogWorkflow::new(
            RuntimePaths {
                catalog: path.clone(),
            },
            Arc::clone(&clock),
            Arc::new(NativeEvents),
            Arc::new(RenderCapability(renderer_available)),
            Arc::new(InlineExecutor),
            wake,
            core_runtime::jobs::Config {
                workers: 2,
                max_jobs: 16,
                max_bytes: 128 * 1024 * 1024,
                max_terminal_records: 128,
            },
        )?;
        workflow.open_initial()?;
        // The mirror follows the catalog the workflow owns: it parks while nothing is open and
        // re-binds itself on every open / close / replacement. Failing to start it costs the user
        // sidecars, not the session, so the error goes to the host's notice path instead of here.
        let (mirror, mirror_error) = match MirrorService::start(workflow.catalog(), mirror_wake) {
            Ok(service) => (Some(service), None),
            Err(error) => {
                eprintln!("sidecar mirror unavailable: {error}");
                (None, Some(error))
            }
        };
        Ok(Self {
            workflow,
            catalog_path: path,
            clock,
            import,
            mirror,
            mirror_error,
        })
    }
    pub fn reopen_command(&self) -> Command {
        Command::OpenCatalog(self.catalog_path.clone())
    }
    pub fn snapshot(&self) -> Arc<ApplicationSnapshot> {
        self.workflow.snapshot()
    }
    pub fn dispatch(&mut self, command: Command) -> Result<(), AppError> {
        // A running import holds the open catalog's connection: end it (the current chunk
        // commits) before the catalog is closed, replaced or the application quits.
        if matches!(
            command,
            Command::CloseCatalog | Command::OpenCatalog(_) | Command::RequestShutdown
        ) {
            self.import.stop_and_join();
        }
        self.workflow.dispatch(command)
    }
    pub fn import_view(&self) -> ImportView {
        self.import.view()
    }
    pub fn handle_import(
        &mut self,
        request: ImportRequest,
        platform: &dyn ImportPlatform,
    ) -> Result<(), AppError> {
        let catalog = self.workflow.catalog();
        self.import
            .handle(request, &catalog, &self.catalog_path, platform)
    }
    /// A commit that finished since the last call; the host refreshes the library on it.
    pub fn take_import_finished(&mut self) -> Option<ImportReport> {
        self.import.take_finished()
    }
    /// A commit ended (finished or failed) since the last call; the host re-reads the library.
    pub fn take_import_refresh(&mut self) -> bool {
        self.import.take_refresh()
    }
    pub fn pump(&mut self) -> Result<(), AppError> {
        while self.workflow.pump()? {}
        // Any completion may have been a committed save or cull mark. Cheap enough to do every
        // time rather than to track which completion applied a commit.
        if let Some(mirror) = &self.mirror {
            mirror.notify();
        }
        self.refresh_mirror_status()?;
        if self.workflow.needs_join() {
            self.workflow.join_shutdown();
        }
        // The quit barrier has flushed every save and the catalog workers are joined, and the
        // workflow only closes the catalog on drop: this is the point where the drain can still
        // finish its in-hand write against an open catalog.
        if self.workflow.is_shutdown_complete() {
            if let Some(mut mirror) = self.mirror.take() {
                mirror
                    .shutdown()
                    .map_err(|error| AppError::Catalog(error.into()))?;
            }
        }
        Ok(())
    }
    fn refresh_mirror_status(&mut self) -> Result<(), AppError> {
        let unsettled = matches!(
            self.workflow.snapshot().editor.save.mirror,
            MirrorStatus::Pending { .. } | MirrorStatus::Failing { .. }
        );
        // Taken only while Pending/Failing is shown, so a signal raised before the save
        // acknowledgement reaches the session is kept until it can act.
        if unsettled && self.mirror.as_ref().is_some_and(|m| m.take_progress()) {
            // A status read refused mid-barrier (quit, catalog replacement) is not fatal to the
            // session: the next drain progress asks again.
            if let Err(error) = self.workflow.dispatch(Command::RefreshMirrorStatus) {
                eprintln!("sidecar status refresh not admitted: {error}");
            }
        }
        Ok(())
    }
    /// Why the sidecar mirror is not running, if it could not be started. `None` while it is (or
    /// was) running. The host shows this in the status area: edits stay saved in the catalog and
    /// reach the photo folders on the next launch.
    pub fn mirror_error(&self) -> Option<&str> {
        self.mirror_error.as_deref()
    }
    pub fn is_shutdown_complete(&self) -> bool {
        self.workflow.is_shutdown_complete()
    }
    pub fn wait_idle(&mut self) -> Result<(), AppError> {
        self.workflow.wait_idle()
    }
    pub fn shutdown(&mut self) -> Result<(), AppError> {
        self.dispatch(Command::RequestShutdown)
    }
    /// When the host must wake to run the next autosave (debounce / backoff), as an `Instant`.
    /// `None` when no timer is pending; an already-due deadline maps to now.
    pub fn next_deadline(&self) -> Option<Instant> {
        let due = self.workflow.next_deadline()?;
        let wait = due.saturating_sub(self.clock.now_millis());
        Instant::now().checked_add(Duration::from_millis(wait))
    }
}
