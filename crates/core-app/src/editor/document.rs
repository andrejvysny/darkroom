//! The ready editor document: params, monotonic revision, bounded history, gesture slot.

use std::sync::Arc;

use core_model::document::EditDocument;
use core_model::identity::{EditRevision, EditorSession, ImageId, ProcessVersion};
use core_model::DevelopParams;

use super::gesture::Gesture;
use super::history::{History, HistoryLimits};
use super::{EditorError, EditorEvent};

/// Consecutive discrete edits within this window of the PREVIOUS discrete edit coalesce into one
/// undo step — a sliding window, so a continuous run of applies each <= 700 ms apart is one step
/// (legacy parity: `UNDO_BURST_MS` / `lastCommitAt` in `src/views/Develop/useDevelop.ts` `commit`).
const BURST_MS: u64 = 700;

/// The full editor state machine; `Ready` is the only mutable state.
#[derive(Debug, Clone)]
pub enum EditorState {
    /// No image open.
    Empty,
    /// Load in flight for `image` under `session`; late results for other sessions are dropped.
    Loading {
        /// Image being loaded.
        image: ImageId,
        /// Session allocated by the preceding `Open`.
        session: EditorSession,
    },
    /// Editable document.
    Ready(Box<ReadyDoc>),
    /// Stored edit exists but this build cannot interpret it. Raw JSON is preserved and every
    /// mutation is refused so it can never be overwritten by accident.
    Unsupported {
        /// Image whose stored edit is unreadable.
        image: ImageId,
        /// Session of the refused load.
        session: EditorSession,
        /// Preserved original document.
        document: EditDocument,
        /// Human-readable refusal reason.
        reason: String,
    },
    /// Stored edit exists but is not parseable JSON / not finite. Preserved, read-only.
    Corrupt {
        /// Image whose stored edit is corrupt.
        image: ImageId,
        /// Session of the refused load.
        session: EditorSession,
        /// Preserved original document.
        document: EditDocument,
    },
    /// Load failed (I/O or backend error).
    Error {
        /// Image whose load failed.
        image: ImageId,
        /// Session of the failed load.
        session: EditorSession,
        /// Failure description.
        message: String,
    },
}

/// Ready document: params + monotonic revision + bounded history + optional live gesture.
#[derive(Debug, Clone)]
pub struct ReadyDoc {
    /// Image this document edits.
    pub image: ImageId,
    /// Session this document belongs to; commands from other sessions are stale.
    pub session: EditorSession,
    /// Carried from the stored document, never changed here.
    pub process_version: ProcessVersion,
    /// Current edit parameters.
    pub params: Arc<DevelopParams>,
    /// Advances on EVERY visible change, never goes back.
    pub revision: EditRevision,
    /// Revision last acknowledged persisted, if any.
    pub saved_revision: Option<EditRevision>,
    history: History,
    gesture: Option<Gesture>,
    last_discrete_at_ms: Option<u64>,
    /// True while consecutive `Apply`s keep coalescing into the sliding burst anchored at the
    /// previous discrete edit (`last_discrete_at_ms`); any non-Apply mutation ends it.
    burst_open: bool,
    initial_revision: EditRevision,
}

impl ReadyDoc {
    pub(crate) fn new(
        image: ImageId,
        session: EditorSession,
        params: Arc<DevelopParams>,
        process_version: ProcessVersion,
        limits: HistoryLimits,
    ) -> Self {
        Self {
            image,
            session,
            process_version,
            params,
            revision: EditRevision(1),
            saved_revision: None,
            history: History::new(limits),
            gesture: None,
            last_discrete_at_ms: None,
            burst_open: false,
            initial_revision: EditRevision(1),
        }
    }

    /// Read-only view of the undo/redo stacks (tests, snapshot building).
    pub fn history(&self) -> &History {
        &self.history
    }

    /// Unsaved-changes predicate: an untouched default/stored doc is clean.
    pub fn is_dirty(&self) -> bool {
        self.revision != self.initial_revision && self.saved_revision != Some(self.revision)
    }

    pub fn gesture_active(&self) -> bool {
        self.gesture.is_some()
    }

    pub(crate) fn begin_gesture(&mut self) -> Result<EditorEvent, EditorError> {
        if self.gesture.is_some() {
            return Err(EditorError::GestureActive);
        }
        self.gesture = Some(Gesture::begin(&self.params));
        self.burst_open = false;
        Ok(EditorEvent::Unchanged)
    }

    pub(crate) fn update_gesture(
        &mut self,
        params: DevelopParams,
    ) -> Result<EditorEvent, EditorError> {
        if self.gesture.is_none() {
            return Err(EditorError::NoGesture);
        }
        let next = self.next_revision()?;
        self.params = Arc::new(params);
        self.revision = next;
        Ok(EditorEvent::Changed {
            revision: self.revision,
            params: self.params.clone(),
        })
    }

    pub(crate) fn commit_gesture(&mut self) -> Result<EditorEvent, EditorError> {
        let gesture = self.gesture.take().ok_or(EditorError::NoGesture)?;
        // The visible change already advanced the revision during updates; commit only
        // records the single pre-gesture snapshot, so the render event is Unchanged.
        if *self.params != *gesture.before {
            self.history.push_undo(gesture.before);
            self.history.clear_redo();
        }
        self.burst_open = false;
        Ok(EditorEvent::Unchanged)
    }

    pub(crate) fn cancel_gesture(&mut self) -> Result<EditorEvent, EditorError> {
        let gesture = self.gesture.take().ok_or(EditorError::NoGesture)?;
        let next = self.next_revision()?;
        // Identity moves forward even when the pixels go back.
        self.params = gesture.before;
        self.revision = next;
        self.burst_open = false;
        Ok(EditorEvent::Changed {
            revision: self.revision,
            params: self.params.clone(),
        })
    }

    pub(crate) fn focus_lost(&mut self) -> Result<EditorEvent, EditorError> {
        if self.gesture.is_some() {
            self.commit_gesture()
        } else {
            Ok(EditorEvent::Unchanged)
        }
    }

    pub(crate) fn apply(
        &mut self,
        params: DevelopParams,
        at_ms: u64,
    ) -> Result<EditorEvent, EditorError> {
        if self.gesture.is_some() {
            return Err(EditorError::GestureActive);
        }
        let next = self.next_revision()?;
        // Sliding window (legacy `commit`): the window is measured from the PREVIOUS discrete
        // edit, so `last_discrete_at_ms` advances on every Apply, not only on a push.
        let coalesces = self.burst_open
            && self
                .last_discrete_at_ms
                .is_some_and(|t| at_ms.saturating_sub(t) <= BURST_MS);
        if !coalesces {
            self.history.push_undo(self.params.clone());
        }
        self.last_discrete_at_ms = Some(at_ms);
        self.burst_open = true;
        self.history.clear_redo();
        self.params = Arc::new(params);
        self.revision = next;
        Ok(EditorEvent::Changed {
            revision: self.revision,
            params: self.params.clone(),
        })
    }

    pub(crate) fn undo(&mut self) -> Result<EditorEvent, EditorError> {
        self.gesture_action(|history, current| {
            let prev = history.pop_undo()?;
            history.push_redo(current.clone());
            Some(prev)
        })
    }

    pub(crate) fn redo(&mut self) -> Result<EditorEvent, EditorError> {
        self.gesture_action(|history, current| {
            let nxt = history.pop_redo()?;
            history.push_undo(current.clone());
            Some(nxt)
        })
    }

    pub(crate) fn reset(&mut self) -> Result<EditorEvent, EditorError> {
        self.set_params(DevelopParams::default())
    }

    pub(crate) fn paste(&mut self, params: DevelopParams) -> Result<EditorEvent, EditorError> {
        self.set_params(params)
    }

    pub(crate) fn mark_saved(
        &mut self,
        revision: EditRevision,
    ) -> Result<EditorEvent, EditorError> {
        // Out-of-order acks of older revisions never regress the saved marker.
        let accepted = revision.0 <= self.revision.0
            && self.saved_revision.is_none_or(|saved| revision.0 > saved.0);
        if !accepted {
            return Ok(EditorEvent::Unchanged);
        }
        self.saved_revision = Some(revision);
        Ok(EditorEvent::Saved(revision))
    }

    pub(crate) fn take_pending_close(
        &mut self,
    ) -> Option<(ImageId, EditRevision, Arc<DevelopParams>, ProcessVersion)> {
        self.commit_active_for_close();
        self.is_dirty().then(|| {
            (
                self.image,
                self.revision,
                self.params.clone(),
                self.process_version,
            )
        })
    }

    pub(crate) fn commit_active_for_close(&mut self) {
        if self.gesture.is_some() {
            // Best-effort: the doc is being discarded, revision is already at capacity
            // only if a prior update overflowed, which cannot leave a live gesture.
            let _ = self.commit_gesture();
        }
    }

    fn set_params(&mut self, params: DevelopParams) -> Result<EditorEvent, EditorError> {
        if self.gesture.is_some() {
            return Err(EditorError::GestureActive);
        }
        let next = self.next_revision()?;
        let params = Arc::new(params);
        // Equal-params Reset/Paste still advance the revision (identity is monotonic) but
        // push nothing: history records changes only.
        if *self.params != *params {
            self.history.push_undo(self.params.clone());
        }
        self.history.clear_redo();
        self.burst_open = false;
        self.params = params;
        self.revision = next;
        Ok(EditorEvent::Changed {
            revision: self.revision,
            params: self.params.clone(),
        })
    }

    fn gesture_action(
        &mut self,
        step: impl FnOnce(&mut History, &Arc<DevelopParams>) -> Option<Arc<DevelopParams>>,
    ) -> Result<EditorEvent, EditorError> {
        if self.gesture.is_some() {
            return Err(EditorError::GestureActive);
        }
        let next = self.next_revision()?;
        let Some(moved) = step(&mut self.history, &self.params) else {
            return Ok(EditorEvent::Unchanged);
        };
        self.burst_open = false;
        self.params = moved;
        self.revision = next;
        Ok(EditorEvent::Changed {
            revision: self.revision,
            params: self.params.clone(),
        })
    }

    fn next_revision(&self) -> Result<EditRevision, EditorError> {
        self.revision.checked_next().ok_or(EditorError::Exhausted)
    }
}
