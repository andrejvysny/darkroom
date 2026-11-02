use crate::state::MirrorStatus;
use crate::AppError;
use core_model::catalog::{
    CollectionRow, DateTreeYear, FolderRow, ImageRow, KeywordRow, PresetSummary, QueryParams,
    RejectSummary, SnapshotSummary,
};
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
    /// Persist one rating / flag / colour-label change for `image_id`.
    fn set_cull(&self, image_id: i64, change: crate::workflow::CullChange) -> Result<(), AppError>;
    fn keywords(&self) -> Result<Vec<KeywordRow>, AppError>;
    fn collections(&self) -> Result<Vec<CollectionRow>, AppError>;
    /// Where the sidecar mirror of `image_id`'s saved edit stands; read-only.
    fn mirror_status(&self, image_id: i64) -> Result<crate::state::MirrorStatus, AppError>;
    /// Keywords, static collections containing the image, and its pair (from either member).
    fn image_tags(&self, image_id: i64) -> Result<crate::state::ImageTags, AppError>;
    /// Counts for deleting the rejected photos matching `params` (reject flag forced on).
    fn reject_summary(&self, params: &QueryParams) -> Result<RejectSummary, AppError>;
    fn presets(&self) -> Result<Vec<PresetSummary>, AppError>;
    /// Named snapshots of `image_id`, newest first.
    fn snapshots(&self, image_id: i64) -> Result<Vec<SnapshotSummary>, AppError>;
    /// Full params for `source` against `base`, blended by `amount` (0..=1) unless the source is a
    /// snapshot (always its full params). `replace_all` starts from defaults instead of `base`.
    /// Nothing is persisted.
    fn resolve_settings(
        &self,
        source: &crate::workflow::SettingsSource,
        base: &core_model::DevelopParams,
        amount: f32,
        replace_all: bool,
    ) -> Result<core_model::DevelopParams, AppError>;
    /// Apply one durable mutation. `DeleteRejected` moves files to the OS Trash with the catalog
    /// lock released between plan and finalize.
    fn mutate(
        &self,
        mutation: &crate::workflow::Mutation,
    ) -> Result<crate::workflow::MutationReport, AppError>;
}

/// What the catalog committed for one `save_edit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveReceipt {
    pub catalog_revision: CatalogRevision,
    pub mirror: MirrorStatus,
    /// The stored `edits.updated_at` after the save (never the supplied timestamp): the edit
    /// version thumbnails are keyed by.
    pub edited_at: i64,
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
