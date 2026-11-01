use crate::editor::{Editor, EditorCommand, EditorError, EditorEvent, EditorState, HistoryLimits};
use crate::ports::{Clock, EventSink, RenderBackend};
use crate::registry::{self, EnabledContext};
use crate::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SettingsSnapshot,
};
use crate::subscription::{Notice, Slot, SnapshotUpdate, Subscription};
use crate::workflow::{Command, Completion, Effect, Operation, Outcome, RequestId, RequestToken};
use crate::AppError;
use core_model::identity::{
    CatalogEpoch, ImageId, ProcessVersion, SelectionGeneration, CURRENT_PROCESS_VERSION,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

pub struct ApplicationSession {
    clock: Arc<dyn Clock>,
    events: Arc<dyn EventSink>,
    snapshot: Arc<ApplicationSnapshot>,
    slots: Vec<Weak<Mutex<Slot>>>,
    pending: HashMap<RequestToken, Operation>,
    next_request: u64,
    /// Authoritative editor document; it allocates `EditorSession`s, so the snapshot's
    /// `editor_session` is always a projection of its live session — never a second counter.
    editor: Editor,
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
            editor: Editor::new(HistoryLimits::default()),
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
        let selection = self
            .selection_generation
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        self.selection_generation = selection;
        self.pending.clear();
        // The outgoing document dies with the catalog. Its unsaved revision is dropped for now:
        // persisting `Closed { pending }` is the autosave brief's job, and until that lands no
        // path may pretend the edit reached storage.
        self.close_editor();
        let s = Arc::make_mut(&mut self.snapshot);
        s.catalog_epoch = epoch;
        s.catalog_ready = false;
        s.library = LibrarySnapshot::default();
        s.editor = EditorSnapshot::default();
        s.error = None;
        Ok(())
    }
    /// Close the editor document, discarding the (not yet persistable) pending revision.
    fn close_editor(&mut self) {
        let _ = self.editor.apply(EditorCommand::Close);
    }
    /// Project the editor's state into the snapshot; call after EVERY editor mutation.
    fn sync_document(&mut self) {
        Arc::make_mut(&mut self.snapshot).editor.document = document_view(self.editor.state());
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
        // A selection carries two effects (the row lookup and the edit load); everything else
        // issues at most one.
        let requests = match &command {
            Command::SelectImage(None) => 0,
            Command::SelectImage(Some(_)) => 2,
            _ => 1,
        };
        self.preflight(requests)?;
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
    fn select(&mut self, id: Option<ImageId>) -> Result<Vec<Effect>, AppError> {
        // Reserve the generation first: if it overflows, neither the editor nor the snapshot moved.
        let generation = self
            .selection_generation
            .checked_next()
            .ok_or(AppError::Exhausted)?;
        // The outgoing document is always closed first, so the editor never holds two documents and
        // the (not yet persistable) pending revision is discarded on every transition.
        self.close_editor();
        let session = match id {
            None => None,
            Some(image_id) => match self.editor.apply(EditorCommand::Open {
                image: image_id,
                // A stored edit keeps its own version; this is the version for an image with no edit.
                default_process_version: CURRENT_PROCESS_VERSION,
            }) {
                Ok(EditorEvent::Opened(session)) => Some(session),
                other => return Err(editor_refusal(image_id, other)),
            },
        };
        self.selection_generation = generation;
        self.pending
            .retain(|_, op| !matches!(op, Operation::SelectImage { .. }));
        let editor_session = session.unwrap_or_default();
        Arc::make_mut(&mut self.snapshot).editor = EditorSnapshot {
            selected: id,
            image: None,
            editor_session,
            selection_generation: generation,
            document: document_view(self.editor.state()),
        };
        let Some(image_id) = id else {
            return Ok(Vec::new());
        };
        // Both effects belong to the same editor session. The row lookup is replaced by a newer
        // selection (only the newest row may reach the UI); the edit load is deliberately left
        // pending so a late result still reaches `complete`, where the editor decides whether it
        // is current or stale.
        Ok(vec![
            self.issue(Operation::SelectImage {
                image_id,
                editor_session,
                selection_generation: generation,
            }),
            self.issue(Operation::LoadEdit {
                image_id,
                editor_session,
            }),
        ])
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
        let effects = match (operation, completion.result) {
            // The editor alone decides whether a load result still matters: a result for a
            // superseded session is dropped silently, because the newer selection owns the
            // document now and must not be overwritten by the old image's params.
            (Operation::LoadEdit { editor_session, .. }, Ok(Outcome::Edit(document))) => {
                if self.editor.current_session() == Some(editor_session) {
                    let _ = self.editor.apply(EditorCommand::Loaded {
                        session: editor_session,
                        stored: document,
                    });
                    self.sync_document();
                }
                Vec::new()
            }
            // A failed load is a document-level failure, not an application error: the user keeps
            // the library and the selection, and the editor shows why this image has no params.
            (Operation::LoadEdit { editor_session, .. }, Err(error)) => {
                if self.editor.current_session() == Some(editor_session) {
                    let _ = self.editor.apply(EditorCommand::LoadFailed {
                        session: editor_session,
                        message: error.to_string(),
                    });
                    self.sync_document();
                }
                Vec::new()
            }
            (_, Err(error)) => {
                Arc::make_mut(&mut self.snapshot).error = Some(error);
                Vec::new()
            }
            (_, Ok(outcome)) => self.apply(outcome),
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
            // `Closed` needs no follow-up work, and `Edit` never reaches here: its result is
            // consumed in `complete`, where the issuing operation's editor session is known.
            Outcome::Closed | Outcome::Edit(_) => Vec::new(),
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
            | (Operation::LoadEdit { .. }, Outcome::Edit(_))
    )
}

/// Project the authoritative editor state into the GPU-free snapshot view. The status is copied
/// rather than inferred: an unreadable stored edit must stay visible as `Unsupported`/`Corrupt` so
/// the renderer can never fall back to defaults and later overwrite it.
fn document_view(state: &EditorState) -> DocumentView {
    match state {
        EditorState::Empty => DocumentView::default(),
        EditorState::Loading { .. } => DocumentView {
            status: DocumentStatus::Loading,
            ..DocumentView::default()
        },
        EditorState::Ready(doc) => DocumentView {
            status: DocumentStatus::Ready,
            params: Some(doc.params.clone()),
            revision: Some(doc.revision),
            process_version: Some(doc.process_version),
            dirty: doc.is_dirty(),
        },
        EditorState::Unsupported {
            document, reason, ..
        } => DocumentView {
            status: DocumentStatus::Unsupported(reason.clone()),
            params: None,
            revision: None,
            process_version: Some(ProcessVersion(document.process_version())),
            dirty: false,
        },
        EditorState::Corrupt { document, .. } => DocumentView {
            status: DocumentStatus::Corrupt,
            params: None,
            revision: None,
            process_version: Some(ProcessVersion(document.process_version())),
            dirty: false,
        },
        EditorState::Error { message, .. } => DocumentView {
            status: DocumentStatus::Error(message.clone()),
            params: None,
            revision: None,
            process_version: None,
            dirty: false,
        },
    }
}

/// The editor only ever refuses an `Open` on session exhaustion; anything else is reported as an
/// invalid command rather than silently leaving the selection without a document.
fn editor_refusal(image_id: ImageId, outcome: Result<EditorEvent, EditorError>) -> AppError {
    match outcome {
        Err(EditorError::Exhausted) => AppError::Exhausted,
        other => AppError::InvalidCommand(format!(
            "editor could not open image {image_id:?}: {other:?}"
        )),
    }
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
        session.selection_generation = SelectionGeneration(u64::MAX);
        let before = session.snapshot();
        assert_eq!(
            session.dispatch(Command::SelectImage(None)),
            Err(AppError::Exhausted)
        );
        assert_eq!(session.snapshot(), before);
        // A refused selection must not have opened (or half-opened) a document.
        assert!(matches!(session.editor.state(), EditorState::Empty));
    }
    #[test]
    fn selection_reserves_room_for_both_effects() {
        let mut session = ApplicationSession::new(Arc::new(Stub), Arc::new(Stub), Arc::new(Stub));
        Arc::make_mut(&mut session.snapshot).catalog_ready = true;
        // One slot left: the row lookup fits, the edit load would not, so nothing is issued.
        session.next_request = u64::MAX - 1;
        let before = session.snapshot();
        assert_eq!(
            session.dispatch(Command::SelectImage(Some(ImageId(1)))),
            Err(AppError::Exhausted)
        );
        assert_eq!(session.snapshot(), before);
        assert!(matches!(session.editor.state(), EditorState::Empty));
        assert!(session.pending.is_empty());
    }
}
