use crate::catalog::CatalogAdapter;
use core_app::ports::{CatalogPort, Clock, EffectExecutor, EventSink, RenderBackend};
use core_app::workflow::{Command, Completion, Effect, Operation, Outcome};
use core_app::{AppError, ApplicationSession};
use core_model::document::{params_to_json, EditDocument};
use std::{collections::VecDeque, path::PathBuf, sync::Arc};

/// Host-supplied paths; opening a catalog never discovers a production profile implicitly.
#[derive(Clone, Debug)]
pub struct RuntimePaths {
    pub catalog: PathBuf,
}

/// First-slice synchronous executor. It owns no detached tasks or threads.
/// The bounded asynchronous supervisor replaces execution policy in DARKROOM-136.
pub struct InlineExecutor;
impl EffectExecutor for InlineExecutor {
    fn execute(&self, effect: Effect, catalog: &dyn CatalogPort) -> Completion {
        let result = match effect.operation {
            Operation::OpenCatalog(path) => catalog.open(&path).map(|()| Outcome::Opened),
            Operation::CloseCatalog => catalog.close().map(|()| Outcome::Closed),
            Operation::QueryLibrary(params) => catalog.query(&params).and_then(|rows| {
                catalog
                    .count(&params)
                    .map(|count| Outcome::Library { rows, count })
            }),
            Operation::LoadFolders => catalog.folders().map(Outcome::Folders),
            Operation::LoadDates => catalog.dates().map(Outcome::Dates),
            Operation::SelectImage { image_id, .. } => catalog
                .image(image_id.0)
                .map(|row| Outcome::Image(row.map(Box::new))),
            Operation::LoadEdit { image_id, .. } => catalog
                .edit(image_id.0)
                .map(|stored| Outcome::Edit(stored.map(|(json, pv)| EditDocument::read(json, pv)))),
            Operation::SaveEdit {
                key,
                params,
                process_version,
                ..
            } => params_to_json(&params)
                .map_err(|e| AppError::Catalog(e.into()))
                .map(|json| (key, json, process_version))
                .and_then(|(key, json, pv)| {
                    catalog.save_edit(key.image_id.0, &key.content_hash, pv.0, &json)
                })
                .map(|receipt| Outcome::Saved {
                    catalog_revision: receipt.catalog_revision,
                    mirror: receipt.mirror,
                }),
        };
        Completion {
            token: effect.token,
            result,
        }
    }
}

/// Single state owner with explicit, ordered effects. UI receives snapshots and typed commands.
pub struct CatalogWorkflow {
    session: ApplicationSession,
    paths: RuntimePaths,
    catalog: CatalogAdapter,
    executor: Arc<dyn EffectExecutor>,
}
impl CatalogWorkflow {
    pub fn new(
        paths: RuntimePaths,
        clock: Arc<dyn Clock>,
        events: Arc<dyn EventSink>,
        renderer: Arc<dyn RenderBackend>,
        executor: Arc<dyn EffectExecutor>,
    ) -> Self {
        Self {
            session: ApplicationSession::new(clock, events, renderer),
            paths,
            catalog: CatalogAdapter::default(),
            executor,
        }
    }
    pub fn snapshot(&self) -> Arc<core_app::state::ApplicationSnapshot> {
        self.session.snapshot()
    }
    pub fn subscribe(&mut self) -> core_app::subscription::Subscription {
        self.session.subscribe()
    }
    pub fn open_initial(&mut self) -> Result<(), AppError> {
        self.dispatch(Command::OpenCatalog(self.paths.catalog.clone()))
    }
    pub fn dispatch(&mut self, command: Command) -> Result<(), AppError> {
        let mut pending: VecDeque<_> = self.session.dispatch(command)?.into();
        let mut first_error = None;
        while let Some(effect) = pending.pop_front() {
            if !self.session.is_current(&effect) {
                continue;
            }
            // No state or subscriber lock is held while the adapter executes IO.
            let completion = self.executor.execute(effect, &self.catalog);
            let error = completion.result.as_ref().err().cloned();
            pending.extend(self.session.complete(completion)?);
            if first_error.is_none() {
                first_error = error;
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}
