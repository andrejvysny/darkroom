//! Rendered (our pipeline, true colours) large previews kept on disk for every photo.
//!
//! [`PreviewService`] is GPU-free: it decides which photo needs which tier, in what order, and
//! publishes the result under the catalog's publication fence. The pixels come from the host
//! through [`PreviewRenderPort`], so this crate never links the GPU stack.

mod queue;
mod service;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

pub use service::PreviewService;

/// One render the host is asked to produce.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewJob {
    pub image_id: i64,
    /// Lower-case hex content hash the catalog holds for the row.
    pub hash: String,
    pub path: PathBuf,
    /// The row's edit version when the job was made (`None`: unedited).
    pub edited_at: Option<i64>,
    /// Longest edge of the preview in pixels.
    pub edge: u32,
}

/// What the host rendered: the large preview plus the 1024 px thumbnail derived from the same
/// pixels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenderedPreview {
    pub preview_jpeg: Vec<u8>,
    pub thumb_jpeg: Vec<u8>,
    /// The edit version the host actually rendered (it re-reads the edit itself); `None` for
    /// default parameters. Publication compares it with the catalog before anything is written.
    pub rendered_edit_version: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreviewRenderError {
    /// This build cannot render the photo or its edit; retrying is pointless until either changes.
    Unsupported(String),
    /// Not now (the host is busy with something more urgent); the id goes to the back of the queue.
    Skip(String),
    /// The render failed; not retried until the edit or process restarts.
    Failed(String),
}

/// The host's GPU render. Called from the service thread only, one job at a time.
pub trait PreviewRenderPort: Send + Sync {
    fn render(&self, job: &PreviewJob) -> Result<RenderedPreview, PreviewRenderError>;
}

/// Cheap, cloneable way to feed and steer a running service (import holds one).
#[derive(Clone)]
pub struct PreviewHandle {
    queue: Arc<queue::Queue>,
}

impl PreviewHandle {
    /// Render `ids` before any bulk work: the newest call first, ids within a call in order.
    pub fn prioritize(&self, ids: &[i64]) {
        self.queue.prioritize(ids);
    }

    /// Append `ids` to the bulk queue (deduplicated; existing tiers are skipped by the worker).
    pub fn enqueue_back(&self, ids: &[i64]) {
        self.queue.enqueue_back(ids);
    }

    /// While paused no new job starts (a job in hand finishes); resuming wakes the worker.
    pub fn set_paused(&self, paused: bool) {
        self.queue.set_paused(paused);
    }

    #[cfg(test)]
    pub(crate) fn pending_ids(&self) -> Vec<i64> {
        self.queue.pending_ids()
    }
}
