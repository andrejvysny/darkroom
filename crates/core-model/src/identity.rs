use serde::{Deserialize, Serialize};

macro_rules! identity {
    ($name:ident, $inner:ty) => {
        #[derive(
            Clone,
            Copy,
            Debug,
            Default,
            Eq,
            Hash,
            Ord,
            PartialOrd,
            PartialEq,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub $inner);
    };
}

macro_rules! revision {
    ($name:ident) => {
        identity!($name, u64);
        impl $name {
            /// Advances without wrapping into an earlier identity.
            pub fn checked_next(self) -> Option<Self> {
                self.0.checked_add(1).map(Self)
            }
        }
    };
}

identity!(ImageId, i64);
identity!(ProcessVersion, i64);

/// The process version this build writes and renders with — the single source of truth.
///
/// It used to live only in `src-tauri`, which left the state machine (which stamps a stored
/// edit's version against "the version of this build") without a canonical value to compare to.
/// The legacy host re-exports it as its own `PROCESS_VERSION`; bump it here and nowhere else.
pub const CURRENT_PROCESS_VERSION: ProcessVersion = ProcessVersion(6);
revision!(SourceRevision);
revision!(EditorSession);
revision!(SelectionGeneration);
revision!(EditRevision);
revision!(RendererVersion);
revision!(CacheVersion);
revision!(ViewportRevision);
revision!(DisplayRevision);
revision!(DeviceEpoch);
revision!(CatalogEpoch);
// Durable `edits.revision` save counter owned by the catalog. A different axis than `EditRevision`
// (editor-local, restarts at 1 every session): never compare the two.
identity!(CatalogRevision, i64);

/// The catalog epoch must change whenever a catalog is reset or replaced.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub catalog_epoch: CatalogEpoch,
    pub image_id: ImageId,
    pub source_revision: SourceRevision,
}

/// How hard a frame was rendered. `Interactive` frames are drag-speed approximations, `Settled`
/// frames are the final-quality result for a revision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum QualityTier {
    Interactive,
    Settled,
}

/// Everything a presented frame depends on. Two frames are interchangeable only if equal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct FrameKey {
    pub identity: RenderIdentity,
    pub viewport_px: (u32, u32),
    pub scale_milli: u32,
    pub quality: QualityTier,
}

// Opaque handle the application layer may hold for "the frame currently on screen".
identity!(FrameHandle, u64);

/// The session and selection generation distinguish repeated selections of the same image.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct RenderIdentity {
    pub source: SourceIdentity,
    pub editor_session: EditorSession,
    pub selection_generation: SelectionGeneration,
    pub edit_revision: EditRevision,
    pub process_version: ProcessVersion,
    pub renderer_version: RendererVersion,
    pub cache_version: CacheVersion,
    pub viewport_revision: ViewportRevision,
    pub display_revision: DisplayRevision,
    pub device_epoch: DeviceEpoch,
}
