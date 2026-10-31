use crate::AppError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
use core_model::identity::{CatalogEpoch, EditorSession, ImageId, SelectionGeneration};
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
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EditorSnapshot {
    pub selected: Option<ImageId>,
    pub image: Option<ImageRow>,
    pub editor_session: EditorSession,
    pub selection_generation: SelectionGeneration,
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
