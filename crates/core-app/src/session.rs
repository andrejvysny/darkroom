use crate::ports::{Clock, EventSink, RenderBackend};
use crate::registry::{self, EnabledContext};
use crate::state::{
    ApplicationSnapshot, EditorSnapshot, JobsSnapshot, LibrarySnapshot, SettingsSnapshot,
};
use crate::subscription::{Notice, Slot, SnapshotUpdate, Subscription};
use crate::workflow::{Command, Completion, Effect, Operation, Outcome, RequestId, RequestToken};
use crate::AppError;
use core_model::identity::{CatalogEpoch, EditorSession, SelectionGeneration};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

pub struct ApplicationSession {
    clock: Arc<dyn Clock>,
    events: Arc<dyn EventSink>,
    snapshot: Arc<ApplicationSnapshot>,
    slots: Vec<Weak<Mutex<Slot>>>,
    pending: HashMap<RequestToken, Operation>,
    next_request: u64,
    editor_session: EditorSession,
    selection_generation: SelectionGeneration,
}
impl ApplicationSession {
    pub fn new(
        clock: Arc<dyn Clock>,
        events: Arc<dyn EventSink>,
        render: Arc<dyn RenderBackend>,
    ) -> Self {
        Self {
            clock,
            events,
            slots: Vec::new(),
            pending: HashMap::new(),
            next_request: 0,
            editor_session: EditorSession::default(),
            selection_generation: SelectionGeneration::default(),
            snapshot: Arc::new(ApplicationSnapshot {
                revision: 0,
                running: true,
                catalog_ready: false,
                catalog_epoch: CatalogEpoch::default(),
                library: LibrarySnapshot::default(),
                editor: EditorSnapshot::default(),
                jobs: JobsSnapshot::default(),
                settings: SettingsSnapshot {
                    renderer_available: render.available(),
                },
                error: None,
            }),
        }
    }
    pub fn update_jobs(
        &mut self,
        records: Vec<crate::jobs::JobSnapshot>,
        metrics: crate::jobs::Metrics,
    ) -> Result<bool, AppError> {
        if self.snapshot.jobs.records.as_ref() == &records && self.snapshot.jobs.metrics == metrics
        {
            return Ok(false);
        }
        self.preflight(0)?;
        let jobs = &mut Arc::make_mut(&mut self.snapshot).jobs;
        jobs.records = Arc::new(records);
        jobs.metrics = metrics;
        self.publish(Notice::JobsChanged);
        Ok(true)
    }
    pub fn snapshot(&self) -> Arc<ApplicationSnapshot> {
        self.snapshot.clone()
    }
    pub fn subscribe(&mut self) -> Subscription {
        self.slots.retain(|weak| {
            weak.upgrade()
                .is_some_and(|slot| !slot.lock().unwrap_or_else(|e| e.into_inner()).closed)
        });
        let update = SnapshotUpdate {
            revision: self.snapshot.revision,
            at_millis: self.clock.now_millis(),
            notice: Notice::Initialized,
            snapshot: self.snapshot.clone(),
        };
        let slot = Arc::new(Mutex::new(Slot {
            update: Some(update),
            closed: false,
        }));
        self.slots.push(Arc::downgrade(&slot));
        Subscription { slot }
    }
    pub fn is_current(&self, effect: &Effect) -> bool {
        self.pending.get(&effect.token) == Some(&effect.operation)
    }
    fn preflight(&self, requests: u64) -> Result<(), AppError> {
        self.snapshot
            .revision
            .checked_add(1)
            .ok_or(AppError::Exhausted)?;
        self.next_request
            .checked_add(requests)
            .ok_or(AppError::Exhausted)?;
        Ok(())
    }
    fn reset(&mut self) -> Result<(), AppError> {
        let epoch = self
            .snapshot
            .catalog_epoch
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        let editor = self
            .editor_session
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        let selection = self
            .selection_generation
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        self.editor_session = editor;
        self.selection_generation = selection;
        self.pending.clear();
        let s = Arc::make_mut(&mut self.snapshot);
        s.catalog_epoch = epoch;
        s.catalog_ready = false;
        s.library = LibrarySnapshot::default();
        s.editor = EditorSnapshot::default();
        s.error = None;
        Ok(())
    }
    fn issue(&mut self, operation: Operation) -> Effect {
        self.next_request += 1;
        let token = RequestToken {
            catalog_epoch: self.snapshot.catalog_epoch,
            request_id: RequestId(self.next_request),
        };
        self.pending.insert(token, operation.clone());
        Effect { token, operation }
    }
    fn replace(&mut self, operation: Operation) -> Effect {
        self.pending.retain(|_, existing| {
            std::mem::discriminant(existing) != std::mem::discriminant(&operation)
        });
        self.issue(operation)
    }
    fn publish(&mut self, notice: Notice) {
        let s = Arc::make_mut(&mut self.snapshot);
        s.revision += 1;
        s.jobs.active_effect_count = self.pending.len();
        let update = SnapshotUpdate {
            revision: s.revision,
            at_millis: self.clock.now_millis(),
            notice,
            snapshot: self.snapshot.clone(),
        };
        self.slots.retain(|weak| {
            let Some(slot) = weak.upgrade() else {
                return false;
            };
            let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
            if slot.closed {
                return false;
            }
            slot.update = Some(update.clone());
            true
        });
        self.events.published(&update);
    }
    pub fn dispatch(&mut self, command: Command) -> Result<Vec<Effect>, AppError> {
        let s = &self.snapshot;
        let context = EnabledContext {
            running: s.running,
            catalog_ready: s.catalog_ready,
            has_selection: s.editor.selected.is_some(),
        };
        if !registry::action(command.action())
            .enabled
            .is_enabled(context)
        {
            return Err(if !s.running {
                AppError::Closed
            } else {
                AppError::NotOpen
            });
        }
        if matches!(&command, Command::OpenCatalog(path) if path.as_os_str().is_empty()) {
            return Err(AppError::InvalidCommand(
                "catalog path must not be empty".into(),
            ));
        }
        if matches!(command, Command::SelectImage(Some(id)) if id.0 <= 0) {
            return Err(AppError::InvalidCommand("image id must be positive".into()));
        }
        self.preflight(u64::from(!matches!(command, Command::SelectImage(None))))?;
        let action = command.action();
        let effects = self.dispatch_valid(command)?;
        Arc::make_mut(&mut self.snapshot).error = None;
        self.publish(Notice::CommandAccepted(action));
        Ok(effects)
    }
    fn dispatch_valid(&mut self, command: Command) -> Result<Vec<Effect>, AppError> {
        let effect = match command {
            Command::OpenCatalog(path) => {
                self.reset()?;
                Some(self.issue(Operation::OpenCatalog(path)))
            }
            Command::CloseCatalog => {
                self.reset()?;
                Some(self.issue(Operation::CloseCatalog))
            }
            Command::RequestShutdown => {
                self.reset()?;
                Arc::make_mut(&mut self.snapshot).running = false;
                Some(self.issue(Operation::CloseCatalog))
            }
            Command::QueryLibrary(query) => {
                let library = &mut Arc::make_mut(&mut self.snapshot).library;
                library.query = *query.clone();
                library.rows = Arc::new(Vec::new());
                library.count = 0;
                Some(self.replace(Operation::QueryLibrary(query)))
            }
            Command::RefreshLibrary => Some(self.replace(Operation::QueryLibrary(Box::new(
                self.snapshot.library.query.clone(),
            )))),
            Command::LoadFolders => Some(self.replace(Operation::LoadFolders)),
            Command::LoadDates => Some(self.replace(Operation::LoadDates)),
            Command::SelectImage(id) => return self.select(id),
        };
        Ok(effect.into_iter().collect())
    }
    fn select(
        &mut self,
        id: Option<core_model::identity::ImageId>,
    ) -> Result<Vec<Effect>, AppError> {
        let generation = self
            .selection_generation
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        let editor = self
            .editor_session
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        self.selection_generation = generation;
        self.editor_session = editor;
        self.pending
            .retain(|_, op| !matches!(op, Operation::SelectImage { .. }));
        Arc::make_mut(&mut self.snapshot).editor = EditorSnapshot {
            selected: id,
            image: None,
            editor_session: self.editor_session,
            selection_generation: generation,
        };
        Ok(id
            .map(|image_id| {
                self.issue(Operation::SelectImage {
                    image_id,
                    editor_session: self.editor_session,
                    selection_generation: generation,
                })
            })
            .into_iter()
            .collect())
    }
    pub fn complete(&mut self, completion: Completion) -> Result<Vec<Effect>, AppError> {
        let operation = self
            .pending
            .get(&completion.token)
            .ok_or(AppError::Stale)?
            .clone();
        if let Ok(outcome) = &completion.result {
            if !matches_outcome(&operation, outcome) {
                return Err(AppError::InvalidCommand(
                    "completion outcome does not match operation".into(),
                ));
            }
        }
        if let (Operation::SelectImage { image_id, .. }, Ok(Outcome::Image(Some(row)))) =
            (&operation, &completion.result)
        {
            if row.id != image_id.0 {
                return Err(AppError::InvalidCommand(
                    "image outcome does not match selection".into(),
                ));
            }
        }
        let notice = Notice::EffectCompleted {
            action: operation.action(),
            result: completion.result.as_ref().map(|_| ()).map_err(Clone::clone),
        };
        let requests = u64::from(matches!(completion.result, Ok(Outcome::Opened))) * 3;
        self.preflight(requests)?;
        self.pending.remove(&completion.token);
        let effects = match completion.result {
            Err(error) => {
                Arc::make_mut(&mut self.snapshot).error = Some(error);
                Vec::new()
            }
            Ok(outcome) => self.apply(outcome),
        };
        self.publish(notice);
        Ok(effects)
    }
    fn apply(&mut self, outcome: Outcome) -> Vec<Effect> {
        match outcome {
            Outcome::Opened => {
                Arc::make_mut(&mut self.snapshot).catalog_ready = true;
                vec![
                    self.replace(Operation::QueryLibrary(Box::new(
                        self.snapshot.library.query.clone(),
                    ))),
                    self.replace(Operation::LoadFolders),
                    self.replace(Operation::LoadDates),
                ]
            }
            Outcome::Closed => Vec::new(),
            Outcome::Library { rows, count } => {
                let library = &mut Arc::make_mut(&mut self.snapshot).library;
                library.rows = Arc::new(rows);
                library.count = count;
                Vec::new()
            }
            Outcome::Folders(rows) => {
                Arc::make_mut(&mut self.snapshot).library.folders = Arc::new(rows);
                Vec::new()
            }
            Outcome::Dates(rows) => {
                Arc::make_mut(&mut self.snapshot).library.dates = Arc::new(rows);
                Vec::new()
            }
            Outcome::Image(row) => {
                Arc::make_mut(&mut self.snapshot).editor.image = row.map(|row| *row);
                Vec::new()
            }
        }
    }
}
fn matches_outcome(operation: &Operation, outcome: &Outcome) -> bool {
    matches!(
        (operation, outcome),
        (Operation::OpenCatalog(_), Outcome::Opened)
            | (Operation::CloseCatalog, Outcome::Closed)
            | (Operation::QueryLibrary(_), Outcome::Library { .. })
            | (Operation::LoadFolders, Outcome::Folders(_))
            | (Operation::LoadDates, Outcome::Dates(_))
            | (Operation::SelectImage { .. }, Outcome::Image(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Stub;
    impl Clock for Stub {
        fn now_millis(&self) -> u64 {
            0
        }
    }
    impl EventSink for Stub {
        fn published(&self, _: &SnapshotUpdate) {}
    }
    impl RenderBackend for Stub {
        fn available(&self) -> bool {
            false
        }
    }
    #[test]
    fn exhausted_counters_do_not_mutate_state() {
        let mut session = ApplicationSession::new(Arc::new(Stub), Arc::new(Stub), Arc::new(Stub));
        session.next_request = u64::MAX;
        let before = session.snapshot();
        assert_eq!(
            session.dispatch(Command::OpenCatalog("test".into())),
            Err(AppError::Exhausted)
        );
        assert_eq!(session.snapshot(), before);
        session.next_request = 0;
        Arc::make_mut(&mut session.snapshot).catalog_epoch = CatalogEpoch(u64::MAX);
        let before = session.snapshot();
        assert_eq!(
            session.dispatch(Command::OpenCatalog("test".into())),
            Err(AppError::Exhausted)
        );
        assert_eq!(session.snapshot(), before);
        Arc::make_mut(&mut session.snapshot).catalog_epoch = CatalogEpoch(0);
        Arc::make_mut(&mut session.snapshot).catalog_ready = true;
        session.editor_session = EditorSession(u64::MAX);
        let before = session.snapshot();
        assert_eq!(
            session.dispatch(Command::SelectImage(None)),
            Err(AppError::Exhausted)
        );
        assert_eq!(session.snapshot(), before);
    }
}
