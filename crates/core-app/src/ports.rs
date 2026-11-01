use crate::state::MirrorStatus;
use crate::AppError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
use core_model::identity::CatalogRevision;
use std::path::Path;

/// Concrete persistence stays behind this read-only first-slice interface.
pub trait CatalogPort: Send + Sync {
    fn open(&self, path: &Path) -> Result<(), AppError>;
    fn close(&self) -> Result<(), AppError>;
    fn query(&self, params: &QueryParams) -> Result<Vec<ImageRow>, AppError>;
    fn count(&self, params: &QueryParams) -> Result<i64, AppError>;
    fn folders(&self) -> Result<Vec<FolderRow>, AppError>;
    fn dates(&self) -> Result<Vec<DateTreeYear>, AppError>;
    fn image(&self, id: i64) -> Result<Option<ImageRow>, AppError>;
    /// Stored edit for `image_id` as `(raw params JSON, stored process_version)`, or `None` when the
    /// image has no edit row. The JSON stays raw: interpretation is the model's job
    /// (`core_model::document::EditDocument::read`), never the catalog's.
    fn edit(&self, image_id: i64) -> Result<Option<(String, i64)>, AppError>;
    /// Commit an edit (and its durable sidecar intent) in one transaction, after verifying the
    /// image's content hash still equals `content_hash` (lowercase hex). A changed source is an
    /// error carrying `source changed — not saved`; nothing is written.
    fn save_edit(
        &self,
        image_id: i64,
        content_hash: &str,
        process_version: i64,
        params_json: &str,
    ) -> Result<SaveReceipt, AppError>;
}

/// What the catalog committed for one `save_edit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub catalog_revision: CatalogRevision,
    pub mirror: MirrorStatus,
}

pub trait Clock: Send + Sync {
    fn now_millis(&self) -> u64;
}

/// Notifications are called after publication; adapters must not mutate session state directly.
pub trait EventSink: Send + Sync {
    fn published(&self, update: &crate::subscription::SnapshotUpdate);
}

/// Capability seam; render request/resource ownership lands with the rendering feature tasks.
pub trait RenderBackend: Send + Sync {
    fn available(&self) -> bool;
}

/// Execute immutable work outside the application's state owner.
pub trait EffectExecutor: Send + Sync {
    fn execute(
        &self,
        effect: crate::workflow::Effect,
        catalog: &dyn CatalogPort,
    ) -> crate::workflow::Completion;
}
