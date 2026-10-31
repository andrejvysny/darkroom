use serde::{Deserialize, Serialize};

macro_rules! identity {
    ($name:ident, $inner:ty) => {
        #[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
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

/// The catalog epoch must change whenever a catalog is reset or replaced.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct SourceIdentity {
    pub catalog_epoch: CatalogEpoch,
    pub image_id: ImageId,
    pub source_revision: SourceRevision,
}

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
