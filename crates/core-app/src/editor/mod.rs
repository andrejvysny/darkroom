//! Authoritative editor document with gesture transactions and bounded history.
//!
//! Port of the legacy editor behaviour (`src/store/develop.ts`,
//! `src/views/Develop/useDevelop.ts`): full-snapshot undo, 700 ms burst coalescing,
//! redo invalidation on new edits, and history cleared on image change. The session
//! layer drives this machine; it is GPU-free and depends only on `core-model`.

mod document;
mod gesture;
mod history;

pub use document::{EditorState, ReadyDoc};
pub use history::{History, HistoryLimits};

use core_model::document::{EditDocument, ReadStatus};
use core_model::identity::{EditRevision, EditorSession, ImageId, ProcessVersion};
use core_model::DevelopParams;

/// A command accepted by [`Editor::apply`]; every session-carrying command is checked
/// against the live session before touching state.
#[derive(Debug, Clone)]
pub enum EditorCommand {
    /// Open an image; allocates a fresh session. The previous document is discarded —
    /// call [`EditorCommand::Close`] first to obtain its unsaved revision as `pending`.
    Open {
        /// Image to open.
        image: ImageId,
        /// Process version for documents with no stored edit.
        default_process_version: ProcessVersion,
    },
    /// Load result; `None` means no stored edit exists.
    Loaded {
        /// Session the load was issued for.
        session: EditorSession,
        /// Stored document, if any.
        stored: Option<EditDocument>,
    },
    /// Load failed with a backend message.
    LoadFailed {
        /// Session the load was issued for.
        session: EditorSession,
        /// Failure description.
        message: String,
    },
    /// Start a continuous drag; snapshot taken as the future undo entry.
    BeginGesture {
        /// Live session.
        session: EditorSession,
    },
    /// Replace params mid-drag without touching history.
    UpdateGesture {
        /// Live session.
        session: EditorSession,
        /// New params for the in-flight drag.
        params: DevelopParams,
    },
    /// Finish a drag; records at most one undo entry.
    CommitGesture {
        /// Live session.
        session: EditorSession,
    },
    /// Escape / pointer cancel: revert to the pre-drag snapshot.
    CancelGesture {
        /// Live session.
        session: EditorSession,
    },
    /// Window/pane focus loss; policy commits an active gesture.
    FocusLost {
        /// Live session.
        session: EditorSession,
    },
    /// Discrete edit; coalesces with the previous one within the burst window.
    Apply {
        /// Live session.
        session: EditorSession,
        /// New params.
        params: DevelopParams,
        /// Edit timestamp, monotonic within the session layer.
        at_ms: u64,
    },
    /// Step back one undo entry.
    Undo {
        /// Live session.
        session: EditorSession,
    },
    /// Step forward one redo entry.
    Redo {
        /// Live session.
        session: EditorSession,
    },
    /// Return to the no-op edit.
    Reset {
        /// Live session.
        session: EditorSession,
    },
    /// Apply another image's params wholesale.
    Paste {
        /// Live session.
        session: EditorSession,
        /// Params pasted from elsewhere.
        params: DevelopParams,
    },
    /// Acknowledge that `revision` reached durable storage.
    MarkSaved {
        /// Live session.
        session: EditorSession,
        /// Persisted revision.
        revision: EditRevision,
    },
    /// Close the editor; dirty docs surface their state as `Closed { pending }`.
    Close,
}

/// Why a command was refused; state is untouched in every refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorError {
    /// Command carries a session other than the live one.
    StaleSession,
    /// No ready document (Empty or Loading).
    NotReady,
    /// Document is Unsupported/Corrupt/Error and must not be mutated.
    Protected,
    /// Gesture update/commit/cancel without an active gesture.
    NoGesture,
    /// Discrete mutation attempted while a gesture is in flight.
    GestureActive,
    /// A revision or session counter would overflow.
    Exhausted,
}

/// Notification produced by a successful command.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorEvent {
    /// Command was a no-op (empty stack, rejected ack, inert lifecycle).
    Unchanged,
    /// A session is now viewable: produced by `Open` (entering Loading) and by
    /// `Loaded`/`LoadFailed` (leaving Loading); inspect [`Editor::state`] for detail.
    Opened(EditorSession),
    /// Visible change; render `params` at `revision`.
    Changed {
        /// New monotonic revision.
        revision: EditRevision,
        /// New parameters.
        params: std::sync::Arc<DevelopParams>,
    },
    /// `revision` was acknowledged as saved.
    Saved(EditRevision),
    /// Editor closed; `pending` carries unsaved state of a dirty Ready doc.
    Closed {
        /// Unsaved (image, revision, params, process version), if the doc was dirty.
        pending: Option<(
            ImageId,
            EditRevision,
            std::sync::Arc<DevelopParams>,
            ProcessVersion,
        )>,
    },
}

/// Authoritative editor document state machine.
pub struct Editor {
    state: EditorState,
    next_session: EditorSession,
    limits: HistoryLimits,
    default_process_version: ProcessVersion,
}

impl Editor {
    /// New editor in the `Empty` state with the given history budgets.
    pub fn new(limits: HistoryLimits) -> Self {
        Self {
            state: EditorState::Empty,
            next_session: EditorSession(0),
            limits,
            default_process_version: ProcessVersion(0),
        }
    }

    /// Current state.
    pub fn state(&self) -> &EditorState {
        &self.state
    }

    /// Apply one command; refusals return an error and leave state untouched.
    pub fn apply(&mut self, cmd: EditorCommand) -> Result<EditorEvent, EditorError> {
        match cmd {
            EditorCommand::Open {
                image,
                default_process_version,
            } => self.open(image, default_process_version),
            EditorCommand::Close => self.close(),
            EditorCommand::Loaded { session, stored } => self.loaded(session, stored),
            EditorCommand::LoadFailed { session, message } => self.load_failed(session, message),
            other => self.mutate(other),
        }
    }

    fn open(
        &mut self,
        image: ImageId,
        default_process_version: ProcessVersion,
    ) -> Result<EditorEvent, EditorError> {
        // An active gesture on the outgoing doc is committed before discard so any
        // history-preserving path sees complete state. Unsaved revisions of the previous
        // doc are exposed only via `Close`; the session layer must close before reopening.
        if let EditorState::Ready(doc) = &mut self.state {
            doc.commit_active_for_close();
        }
        let session = self
            .next_session
            .checked_next()
            .ok_or(EditorError::Exhausted)?;
        self.next_session = session;
        self.default_process_version = default_process_version;
        self.state = EditorState::Loading { image, session };
        Ok(EditorEvent::Opened(session))
    }

    fn close(&mut self) -> Result<EditorEvent, EditorError> {
        let previous = std::mem::replace(&mut self.state, EditorState::Empty);
        match previous {
            EditorState::Empty => Ok(EditorEvent::Unchanged),
            EditorState::Ready(mut doc) => {
                let pending = doc.take_pending_close();
                Ok(EditorEvent::Closed { pending })
            }
            _ => Ok(EditorEvent::Closed { pending: None }),
        }
    }

    fn loaded(
        &mut self,
        session: EditorSession,
        stored: Option<EditDocument>,
    ) -> Result<EditorEvent, EditorError> {
        if self.current_session() != Some(session) {
            return Err(EditorError::StaleSession);
        }
        let EditorState::Loading { image, .. } = &self.state else {
            // Defensive: a second result for the same session must not clobber Ready state.
            return Ok(EditorEvent::Unchanged);
        };
        let image = *image;
        self.state = match stored {
            None => self.ready_state(
                image,
                session,
                DevelopParams::default(),
                self.default_process_version,
            ),
            Some(document) => {
                let status = document.status().clone();
                match status {
                    ReadStatus::Supported => {
                        let params = document.preview().cloned().unwrap_or_default();
                        self.ready_state(
                            image,
                            session,
                            params,
                            ProcessVersion(document.process_version()),
                        )
                    }
                    ReadStatus::Invalid => EditorState::Corrupt {
                        image,
                        session,
                        document,
                    },
                    _ => EditorState::Unsupported {
                        image,
                        session,
                        reason: refusal_reason(&status, document.process_version()),
                        document,
                    },
                }
            }
        };
        Ok(EditorEvent::Opened(session))
    }

    fn ready_state(
        &self,
        image: ImageId,
        session: EditorSession,
        params: DevelopParams,
        process_version: ProcessVersion,
    ) -> EditorState {
        EditorState::Ready(Box::new(ReadyDoc::new(
            image,
            session,
            std::sync::Arc::new(params),
            process_version,
            self.limits,
        )))
    }

    fn load_failed(
        &mut self,
        session: EditorSession,
        message: String,
    ) -> Result<EditorEvent, EditorError> {
        if self.current_session() != Some(session) {
            return Err(EditorError::StaleSession);
        }
        let EditorState::Loading { image, .. } = &self.state else {
            return Ok(EditorEvent::Unchanged);
        };
        let image = *image;
        self.state = EditorState::Error {
            image,
            session,
            message,
        };
        Ok(EditorEvent::Opened(session))
    }

    fn mutate(&mut self, cmd: EditorCommand) -> Result<EditorEvent, EditorError> {
        let Some(current) = self.current_session() else {
            // Empty: there is no live session to be stale against; the doc simply is not ready.
            return Err(EditorError::NotReady);
        };
        if cmd.session() != Some(current) {
            return Err(EditorError::StaleSession);
        }
        match &mut self.state {
            EditorState::Empty | EditorState::Loading { .. } => Err(EditorError::NotReady),
            EditorState::Unsupported { .. }
            | EditorState::Corrupt { .. }
            | EditorState::Error { .. } => Err(EditorError::Protected),
            EditorState::Ready(doc) => dispatch(doc, cmd),
        }
    }

    /// The session the live document belongs to, or `None` when nothing is open. Callers outside the
    /// module use this to drop results issued for a superseded session.
    pub fn current_session(&self) -> Option<EditorSession> {
        match &self.state {
            EditorState::Empty => None,
            EditorState::Loading { session, .. } => Some(*session),
            EditorState::Ready(doc) => Some(doc.session),
            EditorState::Unsupported { session, .. }
            | EditorState::Corrupt { session, .. }
            | EditorState::Error { session, .. } => Some(*session),
        }
    }
}

impl EditorCommand {
    fn session(&self) -> Option<EditorSession> {
        match self {
            Self::Open { .. } | Self::Close => None,
            Self::Loaded { session, .. }
            | Self::LoadFailed { session, .. }
            | Self::BeginGesture { session }
            | Self::UpdateGesture { session, .. }
            | Self::CommitGesture { session }
            | Self::CancelGesture { session }
            | Self::FocusLost { session }
            | Self::Apply { session, .. }
            | Self::Undo { session }
            | Self::Redo { session }
            | Self::Reset { session }
            | Self::Paste { session, .. }
            | Self::MarkSaved { session, .. } => Some(*session),
        }
    }
}

/// Human-readable cause for the two `Unsupported` statuses; the other arms are
/// unreachable because callers dispatch on status first.
fn refusal_reason(status: &ReadStatus, process_version: i64) -> String {
    match status {
        ReadStatus::UnsupportedVersion => {
            format!("process version {process_version} is not readable by this build")
        }
        ReadStatus::UnsupportedFields(fields) => {
            format!("unknown edit fields: {}", fields.join(", "))
        }
        ReadStatus::Invalid | ReadStatus::Supported => "document not readable by this build".into(),
    }
}

fn dispatch(doc: &mut ReadyDoc, cmd: EditorCommand) -> Result<EditorEvent, EditorError> {
    match cmd {
        EditorCommand::BeginGesture { .. } => doc.begin_gesture(),
        EditorCommand::UpdateGesture { params, .. } => doc.update_gesture(params),
        EditorCommand::CommitGesture { .. } => doc.commit_gesture(),
        EditorCommand::CancelGesture { .. } => doc.cancel_gesture(),
        EditorCommand::FocusLost { .. } => doc.focus_lost(),
        EditorCommand::Apply { params, at_ms, .. } => doc.apply(params, at_ms),
        EditorCommand::Undo { .. } => doc.undo(),
        EditorCommand::Redo { .. } => doc.redo(),
        EditorCommand::Reset { .. } => doc.reset(),
        EditorCommand::Paste { params, .. } => doc.paste(params),
        EditorCommand::MarkSaved { revision, .. } => doc.mark_saved(revision),
        // Lifecycle commands never reach this path (handled before dispatch).
        EditorCommand::Open { .. }
        | EditorCommand::Close
        | EditorCommand::Loaded { .. }
        | EditorCommand::LoadFailed { .. } => Ok(EditorEvent::Unchanged),
    }
}
