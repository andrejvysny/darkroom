//! Value types of the autosave coordinator.

use core_model::identity::{
    CatalogEpoch, CatalogRevision, EditRevision, EditorSession, ImageId, ProcessVersion,
};
use core_model::DevelopParams;
use std::sync::Arc;

/// Identity of one saveable document: stable across editor sessions, distinct per catalog epoch
/// and per source file (a replaced file under the same id has a different hash).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SaveKey {
    pub catalog_epoch: CatalogEpoch,
    pub image_id: ImageId,
    /// Lowercase hex content hash of the source the edit was made against.
    pub content_hash: String,
}

/// Coordinator-owned monotonic save ordinal; one counter for the whole coordinator, never reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SaveSeq(pub u64);

/// Editor-local position a save was recorded from. Only meaningful relative to the live session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOrigin {
    pub session: EditorSession,
    pub revision: EditRevision,
}

/// One save the host must execute (catalog commit of exactly this seq).
#[derive(Clone, Debug)]
pub struct SaveRequest {
    pub key: SaveKey,
    /// Acknowledgement is seq-exact: report this back in [`Autosave::completed`].
    pub seq: SaveSeq,
    pub origin: EditOrigin,
    /// Parameters captured at dispatch time (latest-wins snapshot).
    pub params: Arc<DevelopParams>,
    /// Process version the params were produced by, carried to the catalog.
    pub process_version: ProcessVersion,
    /// `record` calls since the last successful save (behavioural log input).
    pub touch_count: u32,
}

/// Why a save failed. `SourceChanged` is permanent: the entry is blocked (never retried) until the
/// user discards it; everything else retries on the backoff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveFailure {
    Retryable(String),
    SourceChanged(String),
}

impl From<String> for SaveFailure {
    fn from(message: String) -> Self {
        Self::Retryable(message)
    }
}

impl From<&str> for SaveFailure {
    fn from(message: &str) -> Self {
        Self::Retryable(message.to_owned())
    }
}

/// What reporting a completion did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completed {
    /// Matched no in-flight save (stray, duplicate, pruned, other epoch): nothing changed.
    Stale,
    Acked(Ack),
    /// Matched the in-flight save, which failed.
    Failed,
}

/// What a successful completion acknowledged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ack {
    pub seq: SaveSeq,
    /// Origin of the acknowledged seq. For the newest seq this is the most recent binding, so a
    /// document reopened from pending state (new session) is the one acknowledged.
    pub origin: EditOrigin,
    pub catalog_revision: CatalogRevision,
}

/// Seqs captured at a moment in time; flushed when every listed key has saved >= its seq.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlushTarget(pub Vec<(SaveKey, SaveSeq)>);

/// Observable pending state for one key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSave {
    pub key: SaveKey,
    /// Newest seq recorded for it.
    pub target: SaveSeq,
    /// Highest seq acknowledged by a successful save, if any.
    pub saved: Option<SaveSeq>,
    /// Consecutive failed save attempts.
    pub failures: u32,
    /// Error message of the most recent failure.
    pub last_error: Option<String>,
}
