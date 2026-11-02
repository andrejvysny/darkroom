//! Import workflow contract between the UI and the host's import service.
//!
//! Import is long-running file IO, so it never runs on the ordered catalog lane: the host owns an
//! import service (see `core_runtime::import`) that executes [`ImportRequest`]s and publishes an
//! immutable [`ImportView`]. The UI reads the view and emits requests; it never names catalog
//! connections, paths it did not receive from the view, or engine types.

use crate::workflow::CullFlag;
use std::path::PathBuf;
use std::sync::Arc;

/// How selected files enter the library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ImportMode {
    /// Copy into the library root under `YYYY/YYYY-MM-DD`, hash-verified.
    Copy,
    /// Copy + verify, then send the source to the OS Trash.
    Move,
    /// Catalog the files where they are; nothing is copied.
    #[default]
    Reference,
}

/// RAW+JPEG (or RAW+HEIF) companions found in the source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Pairing {
    /// Link companions to their RAW: one grid cell per shot.
    #[default]
    Pair,
    /// Import every file as its own photo.
    Standalone,
}

/// Duplicate state of one source file (from the background hash scan).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FileStatus {
    /// Not hashed yet.
    #[default]
    Pending,
    New,
    /// Byte-identical to a photo already in the catalog.
    DuplicateLibrary,
    /// Byte-identical to an earlier file in this same source.
    DuplicateBatch,
}

/// One importable file found in the source folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportFile {
    pub path: String,
    pub filename: String,
    pub size_bytes: i64,
    /// Modification time, epoch seconds (0 when unknown); the dialog groups by its UTC day.
    pub mtime: i64,
    pub status: FileStatus,
    /// "raw" | "jpeg" | "png" | "heif" | "hdr".
    pub kind: String,
    /// Shared stem key for RAW+JPEG companions.
    pub pair_key: Option<String>,
    /// "primary" | "secondary" when `pair_key` is set.
    pub pair_role: Option<String>,
}

/// Marks applied to every imported photo after the import commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// Stars 1..=5.
    pub rating: Option<u8>,
    pub flag: Option<CullFlag>,
    pub keywords: Vec<String>,
    pub collection_id: Option<i64>,
    pub new_collection: Option<String>,
}

/// A fully captured import: the UI's selection and settings at the moment Import was pressed.
/// Later UI filter changes cannot alter a running import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub source: PathBuf,
    pub mode: ImportMode,
    /// Library root for Copy/Move (dated subfolders are created below it); ignored for Reference.
    pub destination: Option<PathBuf>,
    /// Absolute paths, each one a `path` from the current [`ImportView::files`].
    pub selected: Vec<String>,
    pub pairing: Pairing,
    pub options: ImportOptions,
}

/// Final per-run counts, reported honestly (partial success is not success).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Import session id; filter the library by it to show exactly this import.
    pub session_id: i64,
    pub total: usize,
    pub added: usize,
    pub skipped: usize,
    pub failed: usize,
    /// Move imports whose source could not be trashed (the copy is in the library).
    pub source_retained: usize,
    pub paired: usize,
    pub unsupported: usize,
    pub not_processed: usize,
    /// Stopped (Stop pressed, catalog closed, or quitting) before every file was processed.
    pub interrupted: bool,
    /// Post-import marks (rating/flag/keywords/collection) failed; the photos are imported.
    pub options_error: Option<String>,
}

/// Where the import service stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ImportPhase {
    #[default]
    Idle,
    /// Enumerating the source folder.
    Listing,
    /// Files listed; the duplicate scan may still be running (see [`ImportView::dedup`]).
    Ready,
    Importing {
        done: usize,
        total: usize,
    },
    /// Stop was requested; waiting for the current chunk to commit.
    Stopping {
        done: usize,
        total: usize,
    },
    Finished(ImportReport),
    Failed(String),
}

/// Immutable import state published by the host each frame.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportView {
    /// The dialog is open.
    pub open: bool,
    pub source: Option<PathBuf>,
    pub recursive: bool,
    /// Listing result for `source` (empty while listing).
    pub files: Arc<Vec<ImportFile>>,
    /// Duplicate-scan progress `(done, total)`; `done == total` when complete.
    pub dedup: (usize, usize),
    pub phase: ImportPhase,
    /// Configured library root (Copy/Move default destination), if any.
    pub library_root: Option<PathBuf>,
    /// Destination chosen with `PickDestination` for this dialog session; `None` = `library_root`.
    pub destination: Option<PathBuf>,
    /// Advances whenever `source`/`files` are replaced, so the UI can reset its selection.
    pub generation: u64,
}

impl ImportView {
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            ImportPhase::Importing { .. } | ImportPhase::Stopping { .. }
        )
    }
}

/// UI → import service requests. The host executes them; none touches the catalog lane directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportRequest {
    /// Open the dialog (loads the library root; keeps the previous source if any).
    Open,
    /// Show the platform folder picker for the source.
    PickSource,
    SetRecursive(bool),
    /// Show the platform folder picker for the Copy/Move destination (dialog-local, not persisted).
    PickDestination,
    Commit(Box<ImportPlan>),
    /// Stop a running import after the current chunk; already-committed files stay imported.
    Stop,
    /// Close the dialog (a running import keeps running; the pill shows progress).
    Close,
    /// Clear a finished/failed result.
    Dismiss,
}
