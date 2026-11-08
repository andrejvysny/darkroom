//! Size + modification time of a cache file as it was read. A corrupt file is deleted only if it
//! is still that same file: a concurrent `ThumbCache::publish` may have renamed a good one into
//! place between the read and the delete.

use std::path::Path;
use std::time::SystemTime;

pub type Stamp = (u64, SystemTime);

pub fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

/// Delete `path` iff it still has the stamp taken before it was read (never when unknown).
pub fn remove_if_unchanged(path: &Path, read_stamp: Option<Stamp>) {
    if read_stamp.is_some() && stamp(path) == read_stamp {
        let _ = std::fs::remove_file(path);
    }
}
