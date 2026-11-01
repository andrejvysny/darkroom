use crate::editor::{EditorError, EditorEvent, EditorState};
use crate::state::{DocumentStatus, DocumentView};
use crate::AppError;
use core_model::identity::{ImageId, ProcessVersion};

/// Project the authoritative editor state into the GPU-free snapshot view. The status is copied
/// rather than inferred: an unreadable stored edit must stay visible as `Unsupported`/`Corrupt` so
/// the renderer can never fall back to defaults and later overwrite it.
pub(super) fn document_view(state: &EditorState) -> DocumentView {
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
            can_undo: doc.history().undo_len() > 0,
            can_redo: doc.history().redo_len() > 0,
        },
        EditorState::Unsupported {
            document, reason, ..
        } => DocumentView {
            status: DocumentStatus::Unsupported(reason.clone()),
            params: None,
            revision: None,
            process_version: Some(ProcessVersion(document.process_version())),
            dirty: false,
            ..DocumentView::default()
        },
        EditorState::Corrupt { document, .. } => DocumentView {
            status: DocumentStatus::Corrupt,
            params: None,
            revision: None,
            process_version: Some(ProcessVersion(document.process_version())),
            dirty: false,
            ..DocumentView::default()
        },
        EditorState::Error { message, .. } => DocumentView {
            status: DocumentStatus::Error(message.clone()),
            params: None,
            revision: None,
            process_version: None,
            dirty: false,
            ..DocumentView::default()
        },
    }
}

/// The editor only ever refuses an `Open` on session exhaustion; anything else is reported as an
/// invalid command rather than silently leaving the selection without a document.
pub(super) fn editor_refusal(
    image_id: ImageId,
    outcome: Result<EditorEvent, EditorError>,
) -> AppError {
    match outcome {
        Err(EditorError::Exhausted) => AppError::Exhausted,
        other => AppError::InvalidCommand(format!(
            "editor could not open image {image_id:?}: {other:?}"
        )),
    }
}
