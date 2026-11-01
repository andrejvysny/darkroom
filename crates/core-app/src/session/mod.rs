//! The application state owner. Split by concern: `edit` (editor commands, autosave bridge and the
//! close/shutdown flush barrier), `view` (snapshot projections), `tests` (unit tests).
mod cull;
mod edit;
#[cfg(test)]
mod tests;
mod view;

use crate::autosave::{Autosave, SaveKey};
use crate::editor::{Editor, EditorCommand, EditorEvent, EditorState, HistoryLimits};
use crate::ports::{Clock, EventSink, RenderBackend};
use crate::registry::{self, EnabledContext};
use crate::state::{
    ApplicationSnapshot, EditorSnapshot, JobsSnapshot, LibrarySnapshot, MirrorStatus, SaveStatus,
    SettingsSnapshot,
};
use crate::subscription::{Notice, Slot, SnapshotUpdate, Subscription};
use crate::workflow::{Command, Completion, Effect, Operation, Outcome, RequestId, RequestToken};
use crate::AppError;
use core_model::identity::{
    CatalogEpoch, CatalogRevision, ImageId, SelectionGeneration, CURRENT_PROCESS_VERSION,
};
use edit::UNSAVED_MESSAGE;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use view::{document_view, editor_refusal};

/// What a pending flush barrier does once every captured save is acknowledged.
enum BarrierKind {
    Open(PathBuf),
    Close,
    Shutdown,
}

/// A catalog close/replace/shutdown held back until the edits it would orphan are saved.
struct Barrier {
    kind: BarrierKind,
    target: crate::autosave::FlushTarget,
    /// Session-clock time the barrier began; it gives up `BARRIER_TIMEOUT_MS` later.
    started_at: u64,
}

/// A flush barrier that has not reached its target after this long is cancelled with
/// `SaveFailed`; it is never treated as flushed.
pub const BARRIER_TIMEOUT_MS: u64 = 30_000;

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
    autosave: Autosave,
    /// Save identity of the selected image; known once its row arrives (the content hash comes
    /// from the row). `None` means edits are refused rather than defaulted.
    key: Option<SaveKey>,
    /// Catalog revision / mirror state last acknowledged for `key` in this epoch.
    saved_revision: Option<CatalogRevision>,
    mirror: MirrorStatus,
    barrier: Option<Barrier>,
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
            autosave: Autosave::new(),
            key: None,
            saved_revision: None,
            mirror: MirrorStatus::Unknown,
            barrier: None,
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
        // The outgoing document dies with the catalog. Unsaved state is never dropped: refuse
        // before touching anything while autosave or the editor still holds any.
        if let Some(error) = self.unsaved_refusal() {
            return Err(error);
        }
        self.selection_generation = selection;
        self.pending.clear();
        self.close_editor();
        if let Err(keys) = self.autosave.retain_epoch(epoch) {
            return Err(self.save_failed(&keys[0], UNSAVED_MESSAGE.into()));
        }
        let s = Arc::make_mut(&mut self.snapshot);
        s.catalog_epoch = epoch;
        s.catalog_ready = false;
        s.library = LibrarySnapshot::default();
        s.editor = EditorSnapshot::default();
        s.error = None;
        Ok(())
    }
    /// Close the editor document, handing any unsaved revision to autosave under the outgoing
    /// document's save identity.
    fn close_editor(&mut self) {
        let session = self.editor.current_session();
        if let Ok(EditorEvent::Closed {
            pending: Some((image, revision, params, pv)),
        }) = self.editor.apply(EditorCommand::Close)
        {
            if let (Some(key), Some(session)) = (self.key.as_ref(), session) {
                if key.image_id == image {
                    let origin = crate::autosave::EditOrigin { session, revision };
                    self.autosave
                        .record(key.clone(), origin, params, pv, self.clock.now_millis());
                }
            }
        }
        self.key = None;
        self.saved_revision = None;
        self.mirror = MirrorStatus::Unknown;
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
            editable: self.key.is_some() && matches!(self.editor.state(), EditorState::Ready(_)),
            can_undo: s.editor.document.can_undo,
            can_redo: s.editor.document.can_redo,
        };
        if !registry::action(command.action())
            .enabled
            .is_enabled(context)
        {
            return Err(self.disabled_error(&command));
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
            Command::SelectImage(Some(_)) | Command::DiscardUnsavedEdits(_) => 2,
            Command::Edit(_) => 0,
            _ => 1,
        };
        self.preflight(requests)?;
        let action = command.action();
        let discarded = match &command {
            Command::DiscardUnsavedEdits(image) => Some(*image),
            _ => None,
        };
        let effects = self.dispatch_valid(command)?;
        Arc::make_mut(&mut self.snapshot).error = None;
        let notice = match discarded {
            Some(image) => Notice::UnsavedEditsDiscarded(image),
            None => Notice::CommandAccepted(action),
        };
        self.publish(notice);
        Ok(effects)
    }
    fn dispatch_valid(&mut self, command: Command) -> Result<Vec<Effect>, AppError> {
        let effect = match command {
            Command::OpenCatalog(path) => return self.begin_replace(BarrierKind::Open(path)),
            Command::CloseCatalog => return self.begin_replace(BarrierKind::Close),
            Command::RequestShutdown => return self.begin_replace(BarrierKind::Shutdown),
            Command::Edit(intent) => return self.edit(intent),
            Command::DiscardUnsavedEdits(image) => return self.discard_unsaved(image),
            Command::SetCull { image, change } => return self.set_cull(image, change),
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
        // Selection is frozen while a flush barrier is pending: a new seq recorded after the
        // barrier's target would be orphaned by the close that follows.
        if self.barrier.is_some() {
            return Err(AppError::EditRefused("catalog is closing".into()));
        }
        // The outgoing document is always closed first, so the editor never holds two documents;
        // its pending revision is handed to autosave, never dropped.
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
            save: SaveStatus::default(),
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
        // A save completion may issue follow-up saves and the deferred close/open (+1).
        let requests = u64::from(matches!(completion.result, Ok(Outcome::Opened))) * 3
            + if matches!(operation, Operation::SaveEdit { .. }) {
                self.autosave.len() as u64 + 1
            } else {
                0
            };
        self.preflight(requests)?;
        self.pending.remove(&completion.token);
        let effects = match (operation, completion.result) {
            // The editor alone decides whether a load result still matters: a result for a
            // superseded session is dropped silently, because the newer selection owns the
            // document now and must not be overwritten by the old image's params.
            (Operation::LoadEdit { editor_session, .. }, Ok(Outcome::Edit(document))) => {
                if self.editor.current_session() == Some(editor_session) {
                    self.finish_load(editor_session, document);
                }
                Vec::new()
            }
            // The row carries the content hash, hence the save identity; it is bound to the
            // selection's own session so a row can never key another selection's document.
            (Operation::SelectImage { editor_session, .. }, Ok(Outcome::Image(row))) => {
                if self.editor.current_session() == Some(editor_session) {
                    self.bind_row(row.as_deref());
                }
                Arc::make_mut(&mut self.snapshot).editor.image = row.map(|row| *row);
                Vec::new()
            }
            (Operation::SaveEdit { key, seq, .. }, result) => self.save_completed(key, seq, result),
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
            Outcome::Closed | Outcome::Edit(_) | Outcome::Saved { .. } | Outcome::Image(_) => {
                Vec::new()
            }
            Outcome::CullApplied { image_id, change } => {
                self.cull_applied(image_id, change);
                Vec::new()
            }
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
            | (Operation::SaveEdit { .. }, Outcome::Saved { .. })
            | (Operation::SetCull { .. }, Outcome::CullApplied { .. })
    )
}
