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

impl SourceRevision {
    /// Revision of a source file's bytes, derived from its hex BLAKE3 content hash so it is
    /// stable across processes: the first 16 hex digits, or an FNV-1a hash of the whole string
    /// when the hash is shorter or not hex. Never 0 for a non-empty hash.
    pub fn from_content_hash(hash: &str) -> Self {
        if hash.is_empty() {
            return Self(0);
        }
        // `get` rather than indexing: a malformed hash with a multi-byte character must not
        // panic on a non-char boundary, it just falls through to FNV-1a.
        let value = hash
            .get(..16)
            .and_then(|prefix| u64::from_str_radix(prefix, 16).ok())
            .unwrap_or_else(|| {
                let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
                for byte in hash.as_bytes() {
                    acc ^= u64::from(*byte);
                    acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
                }
                acc
            });
        // 0 means "unknown source"; mapping a hash there would reintroduce the stale frame.
        Self(if value == 0 { 1 } else { value })
    }
}

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

#[cfg(test)]
mod tests {
    use super::SourceRevision;

    #[test]
    fn a_hex_hash_revises_by_its_first_16_digits() {
        assert_eq!(
            SourceRevision::from_content_hash("00000000000000ff0123456789abcdef"),
            SourceRevision(255)
        );
        assert_eq!(
            SourceRevision::from_content_hash("deadbeefcafebabe0123456789abcdef"),
            SourceRevision(0xdeadbeefcafebabe)
        );
    }

    #[test]
    fn different_hashes_revise_differently() {
        let a = SourceRevision::from_content_hash("00000000000000ff0123456789abcdef");
        let b = SourceRevision::from_content_hash("00000000000000fe0123456789abcdef");
        let c = SourceRevision::from_content_hash("00000000000000fd0123456789abcdef");
        assert!(a != b && b != c && a != c);
    }

    #[test]
    fn a_short_or_non_hex_hash_revises_by_fnv1a() {
        assert_eq!(
            SourceRevision::from_content_hash("nothex"),
            SourceRevision(0xa0d1_003d_b770_2d33)
        );
        assert_eq!(
            SourceRevision::from_content_hash("nothex"),
            SourceRevision::from_content_hash("nothex")
        );
        assert_ne!(
            SourceRevision::from_content_hash("nothex"),
            SourceRevision::from_content_hash("nothexy")
        );
        // 15 hex digits: too short for the prefix path, so the whole string hashes instead.
        assert_eq!(
            SourceRevision::from_content_hash("00000000000000f"),
            SourceRevision(0xc873_8c44_d825_90c1)
        );
        // Long enough but not hex: the prefix parse fails, so again the whole string.
        assert_eq!(
            SourceRevision::from_content_hash("notahexhash00000000"),
            SourceRevision(0x5d28_ff02_bee3_b3ca)
        );
        // A value whose high bit is set survives as-is (no accidental truncation to i64/u32).
        assert_eq!(
            SourceRevision::from_content_hash("ffffffffffffffff0000000000000000"),
            SourceRevision(u64::MAX)
        );
    }

    #[test]
    fn only_an_empty_hash_stays_the_unknown_revision() {
        assert_eq!(SourceRevision::from_content_hash(""), SourceRevision(0));
        // An all-zero prefix hashes to 0; it must not be read back as "unknown".
        assert_ne!(
            SourceRevision::from_content_hash("00000000000000000123456789abcdef"),
            SourceRevision(0)
        );
    }
}
