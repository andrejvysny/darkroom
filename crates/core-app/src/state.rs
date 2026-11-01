use crate::AppError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
use core_model::identity::{
    CatalogEpoch, EditRevision, EditorSession, ImageId, ProcessVersion, SelectionGeneration,
};
use core_model::DevelopParams;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationSnapshot {
    pub revision: u64,
    pub running: bool,
    pub catalog_ready: bool,
    pub catalog_epoch: CatalogEpoch,
    pub library: LibrarySnapshot,
    pub editor: EditorSnapshot,
    pub jobs: JobsSnapshot,
    pub settings: SettingsSnapshot,
    pub error: Option<AppError>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LibrarySnapshot {
    pub query: QueryParams,
    pub rows: Arc<Vec<ImageRow>>,
    pub count: i64,
    pub folders: Arc<Vec<FolderRow>>,
    pub dates: Arc<Vec<DateTreeYear>>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DocumentStatus {
    /// No image is open.
    #[default]
    Empty,
    /// The stored edit is being read.
    Loading,
    /// Readable document; `params` are the values to render.
    Ready,
    /// A stored edit exists but this build cannot interpret it; the reason names why.
    Unsupported(String),
    /// A stored edit exists but is not parseable / not finite.
    Corrupt,
    /// The load itself failed; `message` is the backend description.
    Error(String),
}

/// GPU-free projection of the authoritative editor document (see `crate::editor`). The renderer
/// reads `params` only when `status` is `Ready`; every non-`Ready` status must be surfaced, because
/// silently falling back to defaults would overwrite a stored edit the user cannot see.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentView {
    pub status: DocumentStatus,
    /// Present only for `Ready`.
    pub params: Option<Arc<DevelopParams>>,
    pub revision: Option<EditRevision>,
    pub process_version: Option<ProcessVersion>,
    pub dirty: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorSnapshot {
    pub selected: Option<ImageId>,
    pub image: Option<ImageRow>,
    pub editor_session: EditorSession,
    pub selection_generation: SelectionGeneration,
    pub document: DocumentView,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct JobsSnapshot {
    pub active_effect_count: usize,
    pub records: Arc<Vec<crate::jobs::JobSnapshot>>,
    pub metrics: crate::jobs::Metrics,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsSnapshot {
    pub renderer_available: bool,
}
