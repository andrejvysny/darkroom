use crate::autosave::{SaveKey, SaveSeq};
use crate::registry::ActionId;
use crate::state::MirrorStatus;
use crate::AppError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
use core_model::document::EditDocument;
use core_model::identity::{
    CatalogEpoch, CatalogRevision, EditorSession, ImageId, ProcessVersion, SelectionGeneration,
};
use core_model::DevelopParams;
use std::path::PathBuf;
use std::sync::Arc;

/// Typed editor commands. The session fills in the live `EditorSession` and the image identity;
/// the caller never names either, so a stale control cannot edit the wrong document.
#[derive(Clone, Debug, PartialEq)]
pub enum EditIntent {
    BeginGesture,
    /// Replace the params mid-drag (full params, as the editor's gesture does).
    UpdateGesture(Box<DevelopParams>),
    CommitGesture,
    CancelGesture,
    Undo,
    Redo,
    /// Discrete exposure edit (EV stops); coalesces with neighbouring discrete edits.
    SetExposure(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    OpenCatalog(PathBuf),
    CloseCatalog,
    QueryLibrary(Box<QueryParams>),
    RefreshLibrary,
    SelectImage(Option<ImageId>),
    LoadFolders,
    LoadDates,
    RequestShutdown,
    Edit(EditIntent),
    /// Explicit user decision: drop the image's unsaved edits (never one that is saving).
    DiscardUnsavedEdits(ImageId),
}
impl Command {
    pub fn action(&self) -> ActionId {
        match self {
            Self::OpenCatalog(_) => ActionId::OpenCatalog,
            Self::CloseCatalog => ActionId::CloseCatalog,
            Self::QueryLibrary(_) => ActionId::QueryLibrary,
            Self::RefreshLibrary => ActionId::RefreshLibrary,
            Self::SelectImage(_) => ActionId::SelectImage,
            Self::LoadFolders => ActionId::LoadFolders,
            Self::LoadDates => ActionId::LoadDates,
            Self::RequestShutdown => ActionId::RequestShutdown,
            Self::DiscardUnsavedEdits(_) => ActionId::DiscardUnsavedEdits,
            Self::Edit(intent) => match intent {
                EditIntent::Undo => ActionId::Undo,
                EditIntent::Redo => ActionId::Redo,
                _ => ActionId::Exposure,
            },
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RequestToken {
    pub catalog_epoch: CatalogEpoch,
    pub request_id: RequestId,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Effect {
    pub token: RequestToken,
    pub operation: Operation,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Operation {
    OpenCatalog(PathBuf),
    CloseCatalog,
    QueryLibrary(Box<QueryParams>),
    LoadFolders,
    LoadDates,
    SelectImage {
        image_id: ImageId,
        editor_session: EditorSession,
        selection_generation: SelectionGeneration,
    },
    /// Fetch the stored edit document of the selected image for `editor_session`. Shares the
    /// `SelectImage` action: it is the second half of opening an editor for a selection, not a
    /// user-facing command, so it needs no menu/shortcut descriptor of its own.
    LoadEdit {
        image_id: ImageId,
        editor_session: EditorSession,
    },
    /// Commit `params` for `key` on the ordered catalog lane. The executor verifies the source's
    /// content hash and commits the edit plus the sidecar intent in one transaction. The seq is
    /// echoed back so the completion is acknowledged exactly. Shares the `Exposure` action: it is
    /// a consequence of editing, not a user-facing command.
    SaveEdit {
        key: SaveKey,
        seq: SaveSeq,
        params: Arc<DevelopParams>,
        process_version: ProcessVersion,
    },
}
impl Operation {
    pub fn action(&self) -> ActionId {
        match self {
            Self::OpenCatalog(_) => ActionId::OpenCatalog,
            Self::CloseCatalog => ActionId::CloseCatalog,
            Self::QueryLibrary(_) => ActionId::QueryLibrary,
            Self::LoadFolders => ActionId::LoadFolders,
            Self::LoadDates => ActionId::LoadDates,
            Self::SelectImage { .. } | Self::LoadEdit { .. } => ActionId::SelectImage,
            Self::SaveEdit { .. } => ActionId::Exposure,
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub token: RequestToken,
    pub result: Result<Outcome, AppError>,
}
#[derive(Clone, Debug, PartialEq)]
// A stored document is legitimately far bigger than `Opened`/`Closed`: it carries the original JSON
// plus the parsed preview. Completions are built once per effect and moved, never copied in a loop,
// so boxing it would add an allocation to every selection to shrink a message that is already
// dropped right after `complete`.
#[allow(clippy::large_enum_variant)]
pub enum Outcome {
    Opened,
    Closed,
    Library {
        rows: Vec<ImageRow>,
        count: i64,
    },
    Folders(Vec<FolderRow>),
    Dates(Vec<DateTreeYear>),
    Image(Option<Box<ImageRow>>),
    /// Stored edit for a `LoadEdit`, already interpreted by the model. `None` means the image has
    /// no edit row, which is a successful load of the default document.
    Edit(Option<EditDocument>),
    /// A `SaveEdit` committed: the durable catalog revision plus the mirror state read in the same
    /// call (never inferred from the catalog save).
    Saved {
        catalog_revision: CatalogRevision,
        mirror: MirrorStatus,
    },
}
