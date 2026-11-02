use crate::autosave::{SaveKey, SaveSeq};
use crate::registry::ActionId;
use crate::state::ImageTags;
use crate::state::MirrorStatus;
use crate::AppError;
use core_model::catalog::{
    CollectionRow, DateTreeYear, FolderRow, ImageRow, KeywordRow, QueryParams, RejectDeleteResult,
    RejectSummary,
};
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
    /// Discrete full-params edit (any control); same path and coalescing as `SetExposure`.
    Apply(Box<DevelopParams>),
}

/// Pick/reject flag as stored in `ratings_flags.flag`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CullFlag {
    Pick,
    Reject,
    None,
}
impl CullFlag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Reject => "reject",
            Self::None => "none",
        }
    }
    /// Unknown stored strings read as `None`, matching the catalog's own normalisation.
    pub fn from_row(value: &str) -> Self {
        match value {
            "pick" => Self::Pick,
            "reject" => Self::Reject,
            _ => Self::None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}
impl ColorLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Yellow => "yellow",
            Self::Green => "green",
            Self::Blue => "blue",
            Self::Purple => "purple",
        }
    }
    pub fn from_row(value: &str) -> Option<Self> {
        match value {
            "red" => Some(Self::Red),
            "yellow" => Some(Self::Yellow),
            "green" => Some(Self::Green),
            "blue" => Some(Self::Blue),
            "purple" => Some(Self::Purple),
            _ => None,
        }
    }
}

/// One culling mark change for one image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CullChange {
    /// Stars 0..=5; anything larger is refused at dispatch.
    Rating(u8),
    Flag(CullFlag),
    Label(Option<ColorLabel>),
}

/// A durable catalog change on the ordered lane. Every target is an explicit image id captured
/// by the UI at the moment of the gesture — never an index into a list that may have changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Mutation {
    /// Batch rating / flag / label; one transaction.
    Cull {
        images: Vec<ImageId>,
        change: CullChange,
    },
    /// Create-or-get keyword `name` (case-insensitive) and tag every image with it.
    AddKeyword {
        images: Vec<ImageId>,
        name: String,
    },
    RemoveKeyword {
        images: Vec<ImageId>,
        keyword_id: i64,
    },
    /// Delete a keyword everywhere.
    DeleteKeyword(i64),
    /// Static collection (with initial members) when `smart` is `None`; smart collection
    /// (predicate only, no members) otherwise.
    CreateCollection {
        name: String,
        smart: Option<Box<QueryParams>>,
        images: Vec<ImageId>,
    },
    RenameCollection {
        id: i64,
        name: String,
    },
    DeleteCollection(i64),
    AddToCollection {
        id: i64,
        images: Vec<ImageId>,
    },
    RemoveFromCollection {
        id: i64,
        images: Vec<ImageId>,
    },
    /// Break the pair containing companion `secondary`; it returns to the grid standalone.
    UnlinkPair {
        secondary: ImageId,
    },
    /// Send every rejected photo matching the filter (plus a rejected RAW's companions) to the OS
    /// Trash and drop its rows. The target set is re-derived in the catalog with the reject flag
    /// forced on; the caller never supplies ids.
    DeleteRejected(Box<QueryParams>),
}

/// What a committed [`Mutation`] reports back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationReport {
    Done,
    CollectionCreated(i64),
    RejectedDeleted(RejectDeleteResult),
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
    /// Rate / flag / label one image; runs on the ordered catalog lane.
    SetCull {
        image: ImageId,
        change: CullChange,
    },
    LoadKeywords,
    LoadCollections,
    /// Keywords, collection membership and pair info of one image (the info panel).
    LoadImageTags(ImageId),
    /// Counts for the delete-rejected confirmation, for exactly this filter.
    LoadRejectSummary(Box<QueryParams>),
    Mutate(Mutation),
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
            Self::SetCull { .. } => ActionId::SetCull,
            Self::LoadKeywords => ActionId::LoadKeywords,
            Self::LoadCollections => ActionId::LoadCollections,
            Self::LoadImageTags(_) => ActionId::LoadImageTags,
            Self::LoadRejectSummary(_) => ActionId::LoadRejectSummary,
            Self::Mutate(_) => ActionId::Mutate,
            Self::Edit(intent) => match intent {
                EditIntent::Undo => ActionId::Undo,
                EditIntent::Redo => ActionId::Redo,
                _ => ActionId::Edit,
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
    /// echoed back so the completion is acknowledged exactly. Shares the `Edit` action: it is
    /// a consequence of editing, not a user-facing command.
    SaveEdit {
        key: SaveKey,
        seq: SaveSeq,
        params: Arc<DevelopParams>,
        process_version: ProcessVersion,
    },
    /// Persist one culling mark change on the ordered catalog lane.
    SetCull {
        image_id: ImageId,
        change: CullChange,
    },
    LoadKeywords,
    LoadCollections,
    LoadImageTags(ImageId),
    LoadRejectSummary(Box<QueryParams>),
    Mutate(Mutation),
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
            Self::SaveEdit { .. } => ActionId::Edit,
            Self::SetCull { .. } => ActionId::SetCull,
            Self::LoadKeywords => ActionId::LoadKeywords,
            Self::LoadCollections => ActionId::LoadCollections,
            Self::LoadImageTags(_) => ActionId::LoadImageTags,
            Self::LoadRejectSummary(_) => ActionId::LoadRejectSummary,
            Self::Mutate(_) => ActionId::Mutate,
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
    /// A `SetCull` committed; the session patches the matching snapshot rows.
    CullApplied {
        image_id: ImageId,
        change: CullChange,
    },
    Keywords(Vec<KeywordRow>),
    Collections(Vec<CollectionRow>),
    ImageTags(Box<ImageTags>),
    RejectSummary(RejectSummary),
    Mutated(MutationReport),
}
