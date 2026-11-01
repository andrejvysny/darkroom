//! Editor commands, the autosave bridge and the close/shutdown flush barrier.
//!
//! Every editor `Changed` event is recorded into [`Autosave`] under the selected image's
//! [`SaveKey`]; `tick` turns due saves into ordered `SaveEdit` effects and completions acknowledge
//! them seq-exactly. Closing, replacing or shutting down the catalog first waits for a flush
//! target; a failed save cancels the operation and leaves the editor usable.
//!
//! Undo history is per editor session and is NOT restored when an image is reopened; only the
//! unsaved params are (the restored document is dirty and its undo step returns to the stored
//! state).

use super::{ApplicationSession, Barrier, BarrierKind, BARRIER_TIMEOUT_MS};
use crate::autosave::{Completed, EditOrigin, SaveFailure, SaveKey, SaveRequest, SaveSeq};
use crate::editor::{EditorCommand, EditorError, EditorEvent, EditorState};
use crate::state::SaveStatus;
use crate::subscription::Notice;
use crate::workflow::{Command, EditIntent, Effect, Operation, Outcome};
use crate::AppError;
use core_model::catalog::ImageRow;
use core_model::document::{validate_finite, EditDocument, ReadStatus};
use core_model::identity::EditorSession;
use core_model::DevelopParams;
use std::sync::Arc;

/// Why a close/replace was refused while edits are still unsaved.
pub(super) const UNSAVED_MESSAGE: &str = "unsaved edits are still pending";
const TIMEOUT_MESSAGE: &str = "Saving did not finish in time; Darkroom stayed open.";
const QUITTING: &str = "Darkroom is quitting";

impl ApplicationSession {
    fn image_label(&self, key: &SaveKey) -> String {
        match &self.snapshot.editor.image {
            Some(row) if row.id == key.image_id.0 => row.filename.clone(),
            _ => format!("image {}", key.image_id.0),
        }
    }

    pub(super) fn save_failed(&self, key: &SaveKey, message: String) -> AppError {
        AppError::SaveFailed {
            image: self.image_label(key),
            message,
        }
    }

    fn changed_message(&self, key: &SaveKey) -> String {
        format!(
            "The photo file {} changed on disk, so these edits can't be saved. \
             Discard the unsaved edits to continue.",
            self.image_label(key)
        )
    }

    /// `Some` while the editor or autosave still holds unsaved work that a reset would drop.
    pub(super) fn unsaved_refusal(&self) -> Option<AppError> {
        let editor_dirty = matches!(self.editor.state(), EditorState::Ready(d) if d.is_dirty());
        if self.autosave.is_idle() && !editor_dirty {
            return None;
        }
        let key = self
            .autosave
            .pending()
            .first()
            .map(|p| p.key.clone())
            .or_else(|| self.key.clone());
        Some(match key {
            Some(key) => self.save_failed(&key, UNSAVED_MESSAGE.into()),
            None => AppError::SaveFailed {
                image: "the open image".into(),
                message: UNSAVED_MESSAGE.into(),
            },
        })
    }

    /// Admission is derived from one fact: closed exactly while a Shutdown barrier is pending or
    /// the session has stopped running. Every barrier transition ends here.
    fn sync_admission(&mut self) {
        let quitting = matches!(
            self.barrier.as_ref().map(|b| &b.kind),
            Some(BarrierKind::Shutdown)
        ) || !self.snapshot.running;
        if quitting {
            self.autosave.stop_admission();
        } else {
            self.autosave.resume_admission();
        }
    }

    /// Typed refusal for an action the registry reports disabled.
    pub(super) fn disabled_error(&self, command: &Command) -> AppError {
        let s = &self.snapshot;
        if !s.running {
            return AppError::Closed;
        }
        if !s.catalog_ready {
            return AppError::NotOpen;
        }
        let Command::Edit(intent) = command else {
            return AppError::NotOpen;
        };
        let reason = match (&s.editor.document.status, &self.key) {
            (crate::state::DocumentStatus::Ready, None) => "image row not loaded yet".to_string(),
            (crate::state::DocumentStatus::Ready, Some(_)) => match intent {
                EditIntent::Undo => "nothing to undo".to_string(),
                EditIntent::Redo => "nothing to redo".to_string(),
                _ => "document not editable".to_string(),
            },
            (other, _) => format!("no editable document ({other:?})"),
        };
        AppError::EditRefused(reason)
    }

    pub(super) fn edit(&mut self, intent: EditIntent) -> Result<Vec<Effect>, AppError> {
        if self.barrier.is_some() {
            return Err(AppError::EditRefused("catalog is closing".into()));
        }
        // Checked before the editor moves: a refused record must not leave an edit that exists
        // only on screen.
        if !self.autosave.admits() {
            return Err(AppError::EditRefused(QUITTING.into()));
        }
        let (Some(key), Some(session)) = (self.key.clone(), self.editor.current_session()) else {
            return Err(AppError::EditRefused("image row not loaded yet".into()));
        };
        let EditorState::Ready(doc) = self.editor.state() else {
            return Err(AppError::EditRefused("no editable document".into()));
        };
        let command = match intent {
            EditIntent::BeginGesture => EditorCommand::BeginGesture { session },
            EditIntent::UpdateGesture(params) => {
                require_finite(&params)?;
                EditorCommand::UpdateGesture {
                    session,
                    params: *params,
                }
            }
            EditIntent::CommitGesture => EditorCommand::CommitGesture { session },
            EditIntent::CancelGesture => EditorCommand::CancelGesture { session },
            EditIntent::Undo => EditorCommand::Undo { session },
            EditIntent::Redo => EditorCommand::Redo { session },
            EditIntent::SetExposure(ev) => {
                let mut params = (*doc.params).clone();
                params.exposure = ev;
                require_finite(&params)?;
                EditorCommand::Apply {
                    session,
                    params,
                    at_ms: self.clock.now_millis(),
                }
            }
        };
        let event = self.editor.apply(command).map_err(refusal)?;
        if let EditorEvent::Changed { revision, params } = event {
            if let EditorState::Ready(doc) = self.editor.state() {
                let origin = EditOrigin { session, revision };
                self.autosave.record(
                    key,
                    origin,
                    params,
                    doc.process_version,
                    self.clock.now_millis(),
                );
            }
        }
        self.sync_document();
        self.sync_save();
        Ok(Vec::new())
    }

    /// Bind the save identity once the selected image's row (hence content hash) is known.
    pub(super) fn bind_row(&mut self, row: Option<&ImageRow>) {
        self.key = row.map(|row| SaveKey {
            catalog_epoch: self.snapshot.catalog_epoch,
            image_id: core_model::identity::ImageId(row.id),
            content_hash: row.content_hash.to_ascii_lowercase(),
        });
        self.sync_save();
    }

    /// Complete a document load. Unsaved state still held by autosave for this save identity wins
    /// over the older stored document, so navigating away and back never loses an edit.
    pub(super) fn finish_load(&mut self, session: EditorSession, stored: Option<EditDocument>) {
        let restorable = stored
            .as_ref()
            .is_none_or(|doc| *doc.status() == ReadStatus::Supported);
        let pending = self
            .key
            .as_ref()
            .filter(|_| restorable)
            .and_then(|key| self.autosave.latest_unsaved(key));
        let _ = self.editor.apply(EditorCommand::Loaded { session, stored });
        if let (Some((params, pending_pv)), Some(key)) = (pending, self.key.clone()) {
            // Paste makes the restored params the live, dirty revision; record rebinds the
            // pending seq to this session so its acknowledgement reaches this document.
            if let Ok(EditorEvent::Changed { revision, params }) =
                self.editor.apply(EditorCommand::Paste {
                    session,
                    params: (*params).clone(),
                })
            {
                // The pending entry's version, not the stored document's: they may differ and
                // the restored state must save as exactly what was pending.
                let origin = EditOrigin { session, revision };
                self.autosave
                    .record(key, origin, params, pending_pv, self.clock.now_millis());
            }
        }
        self.sync_document();
        self.sync_save();
    }

    /// Project the selected image's durability into the snapshot.
    pub(super) fn sync_save(&mut self) {
        let save = match &self.key {
            None => SaveStatus::default(),
            Some(key) => SaveStatus {
                dirty: self.snapshot.editor.document.dirty,
                saving: self.autosave.is_saving(key),
                saved_catalog_revision: self.saved_revision,
                last_error: self.autosave.failure(key).map(|(_, message)| message),
                mirror: self.mirror.clone(),
            },
        };
        let editor = &mut Arc::make_mut(&mut self.snapshot).editor;
        if editor.save != save {
            editor.save = save;
        }
    }

    /// Earliest time `tick` could issue a save, for the host's wake timer.
    pub fn next_deadline(&self) -> Option<u64> {
        let barrier = self
            .barrier
            .as_ref()
            .map(|b| b.started_at.saturating_add(BARRIER_TIMEOUT_MS));
        match (self.autosave.next_deadline(), barrier) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Issue the autosave saves that are due at `now_ms` as ordered effects.
    pub fn tick(&mut self, now_ms: u64) -> Result<Vec<Effect>, AppError> {
        if let Some(barrier) = &self.barrier {
            if now_ms >= barrier.started_at.saturating_add(BARRIER_TIMEOUT_MS) {
                // Unflushed means no recorded ack reaches the target: a timeout never counts
                // as flushed.
                let stuck = barrier
                    .target
                    .0
                    .iter()
                    .find(|(k, s)| !self.autosave.is_flushed_one(k, *s))
                    .map(|(k, _)| k.clone());
                if let Some(key) = stuck {
                    self.cancel_barrier(&key, TIMEOUT_MESSAGE.into());
                    self.publish(Notice::BarrierTimedOut);
                }
            }
            return Ok(Vec::new());
        }
        if !self.snapshot.catalog_ready || self.autosave.is_empty() {
            return Ok(Vec::new());
        }
        if self.autosave.next_deadline().is_none_or(|d| d > now_ms) {
            return Ok(Vec::new());
        }
        self.preflight(self.autosave.len() as u64)?;
        let requests = self.autosave.due(now_ms);
        let effects = self.issue_saves(requests);
        if !effects.is_empty() {
            self.sync_save();
            self.publish(Notice::SavesDispatched);
        }
        Ok(effects)
    }

    fn issue_saves(&mut self, requests: Vec<SaveRequest>) -> Vec<Effect> {
        requests
            .into_iter()
            .map(|r| {
                self.issue(Operation::SaveEdit {
                    key: r.key,
                    seq: r.seq,
                    params: r.params,
                    process_version: r.process_version,
                })
            })
            .collect()
    }

    pub(super) fn save_completed(
        &mut self,
        key: SaveKey,
        seq: SaveSeq,
        result: Result<Outcome, AppError>,
    ) -> Vec<Effect> {
        let now = self.clock.now_millis();
        let effects = match result {
            Ok(Outcome::Saved {
                catalog_revision,
                mirror,
            }) => match self
                .autosave
                .completed_outcome(&key, seq, Ok(catalog_revision), now)
            {
                Completed::Acked(ack) => {
                    if self.key.as_ref() == Some(&key) {
                        self.saved_revision = Some(ack.catalog_revision);
                        self.mirror = mirror;
                        // Only the live session's document may be marked saved: a delayed
                        // completion from an earlier session leaves the new one dirty.
                        if self.editor.current_session() == Some(ack.origin.session) {
                            let _ = self.editor.apply(EditorCommand::MarkSaved {
                                session: ack.origin.session,
                                revision: ack.origin.revision,
                            });
                        }
                    }
                    self.advance_barrier()
                }
                Completed::Stale | Completed::Failed => Vec::new(),
            },
            other => {
                let (failure, message) = match other {
                    Err(AppError::SourceChanged) => {
                        let m = self.changed_message(&key);
                        (SaveFailure::SourceChanged(m.clone()), m)
                    }
                    Err(error) => (SaveFailure::Retryable(error.to_string()), error.to_string()),
                    Ok(_) => (
                        SaveFailure::Retryable("unexpected save outcome".into()),
                        "unexpected save outcome".to_string(),
                    ),
                };
                // Only a failure of the save actually in flight cancels the barrier; a stale
                // completion is ignored entirely.
                let outcome = self
                    .autosave
                    .completed_outcome(&key, seq, Err(failure), now);
                if outcome == Completed::Failed {
                    self.cancel_barrier(&key, message);
                }
                Vec::new()
            }
        };
        self.sync_document();
        self.sync_save();
        effects
    }

    /// Start a close/replace/shutdown. Unsaved edits are flushed first; the operation runs once
    /// the captured target is acknowledged. Shutdown is sticky: once requested, a different
    /// request is refused rather than downgrading it.
    pub(super) fn begin_replace(&mut self, kind: BarrierKind) -> Result<Vec<Effect>, AppError> {
        if let Some(barrier) = &mut self.barrier {
            match (&barrier.kind, &kind) {
                (BarrierKind::Shutdown, BarrierKind::Shutdown) => return Ok(Vec::new()),
                (BarrierKind::Shutdown, _) => {
                    return Err(AppError::EditRefused(QUITTING.into()));
                }
                _ => {}
            }
            barrier.kind = kind; // the newest request decides what happens after the flush
            self.sync_admission();
            return Ok(Vec::new());
        }
        let target = self.autosave.flush_target();
        if target.0.is_empty() {
            return self.proceed(kind);
        }
        if let Some(key) = self.autosave.blocked_keys().first() {
            // A changed source can never be saved: waiting would hang, so say so now.
            return Err(self.save_failed(key, self.changed_message(key)));
        }
        self.preflight(self.autosave.len() as u64 + 1)?;
        self.barrier = Some(Barrier {
            kind,
            target,
            started_at: self.clock.now_millis(),
        });
        self.sync_admission();
        let requests = self.autosave.due_now();
        let effects = self.issue_saves(requests);
        self.sync_save();
        Ok(effects)
    }

    fn advance_barrier(&mut self) -> Vec<Effect> {
        let Some(barrier) = &self.barrier else {
            return Vec::new();
        };
        if self.autosave.is_flushed(&barrier.target) {
            let Some(barrier) = self.barrier.take() else {
                return Vec::new();
            };
            return match self.proceed(barrier.kind) {
                Ok(effects) => effects,
                Err(error) => {
                    // The reset refused (unsaved work appeared): stay open and say so.
                    self.sync_admission();
                    Arc::make_mut(&mut self.snapshot).error = Some(error);
                    Vec::new()
                }
            };
        }
        let requests = self.autosave.due_now();
        self.issue_saves(requests)
    }

    fn cancel_barrier(&mut self, key: &SaveKey, message: String) {
        if self.barrier.take().is_none() {
            return;
        }
        self.sync_admission();
        let error = self.save_failed(key, message);
        Arc::make_mut(&mut self.snapshot).error = Some(error);
    }

    fn proceed(&mut self, kind: BarrierKind) -> Result<Vec<Effect>, AppError> {
        self.reset()?;
        let effect = match kind {
            BarrierKind::Open(path) => self.issue(Operation::OpenCatalog(path)),
            BarrierKind::Close => self.issue(Operation::CloseCatalog),
            BarrierKind::Shutdown => {
                Arc::make_mut(&mut self.snapshot).running = false;
                self.issue(Operation::CloseCatalog)
            }
        };
        self.sync_admission();
        Ok(vec![effect])
    }

    /// Explicit user decision to drop an image's unsaved edits. A save already in flight cannot
    /// be recalled, so that is refused. An open document is reloaded from the catalog so the
    /// screen matches storage.
    pub(super) fn discard_unsaved(
        &mut self,
        image: core_model::identity::ImageId,
    ) -> Result<Vec<Effect>, AppError> {
        if self.barrier.is_some() {
            return Err(AppError::EditRefused("catalog is closing".into()));
        }
        let keys = self
            .autosave
            .keys_for_image(self.snapshot.catalog_epoch, image);
        if keys.is_empty() {
            return Err(AppError::EditRefused(
                "no unsaved edits for that image".into(),
            ));
        }
        if !self.autosave.discard(&keys).is_empty() {
            return Err(AppError::EditRefused(
                "a save for that image is in flight; try again when it finishes".into(),
            ));
        }
        if self.key.as_ref().is_some_and(|k| k.image_id == image) {
            let _ = self.editor.apply(EditorCommand::Close);
            self.key = None;
            return self.select(Some(image));
        }
        self.sync_save();
        Ok(Vec::new())
    }
}

fn require_finite(params: &DevelopParams) -> Result<(), AppError> {
    validate_finite(params)
        .then_some(())
        .ok_or_else(|| AppError::EditRefused("non-finite edit value".into()))
}

fn refusal(error: EditorError) -> AppError {
    match error {
        EditorError::Exhausted => AppError::Exhausted,
        other => AppError::EditRefused(format!("{other:?}")),
    }
}
