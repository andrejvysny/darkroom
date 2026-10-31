use crate::AppError;
use core_model::catalog::{DateTreeYear, FolderRow, ImageRow, QueryParams};
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
