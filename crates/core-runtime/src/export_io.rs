//! File-system rules for photo export, shared by the native worker and the legacy Tauri command.
//!
//! Export never overwrites a deliverable and never publishes over a catalog source; both rules
//! live here so the two hosts cannot drift apart.

use chrono::{Local, TimeZone};
use std::io::Write;
use std::path::Path;

/// Write `bytes` to `dest` atomically: a unique `.darkroom-export-*.part` sibling first, then an
/// atomic rename into place. `replace` picks between replacing an existing `dest` and refusing it.
///
/// A half-written export is indistinguishable from a finished one to the user browsing the folder
/// (a truncated JPEG still decodes to a partial image), so the destination either does not exist
/// or is complete; flushing and syncing must succeed before the rename.
pub fn write_atomic_with_policy(dest: &Path, bytes: &[u8], replace: bool) -> Result<(), String> {
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut part = tempfile::Builder::new()
        .prefix(".darkroom-export-")
        .suffix(".part")
        .tempfile_in(parent)
        .map_err(|e| e.to_string())?;
    part.write_all(bytes).map_err(|e| e.to_string())?;
    part.as_file().sync_all().map_err(|e| e.to_string())?;
    (if replace {
        part.persist(dest)
    } else {
        part.persist_noclobber(dest)
    })
    .map(|_| ())
    .map_err(|e| e.error.to_string())
}

/// Batch photo export has no per-file overwrite confirmation; collisions must preserve both the
/// existing photograph and any concurrent writer's output.
pub fn write_new_export(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic_with_policy(dest, bytes, false)
}

/// Refuse `dest` when it is, or resolves to, any catalog source path (another image, a symlink
/// alias, a `..` spelling).
pub fn ensure_not_source(dest: &Path, sources: &[String]) -> Result<(), String> {
    // A missing original still owns its catalog path. Resolve its parent so aliases and `..`
    // cannot publish unrelated pixels under a missing row's old content identity.
    let resolve = |path: &Path| {
        path.canonicalize().ok().or_else(|| {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Some(parent.canonicalize().ok()?.join(path.file_name()?))
        })
    };
    let resolved = resolve(dest);
    if sources.iter().any(|source| {
        let source = Path::new(source);
        source == dest
            || resolved
                .as_ref()
                .is_some_and(|dest| resolve(source).as_ref() == Some(dest))
    }) {
        return Err(
            "export destination is a catalog source image; choose a different filename".into(),
        );
    }
    Ok(())
}

/// Epoch seconds -> local `YYYY-MM-DD` (the `{date}` export token).
pub fn local_ymd(epoch_secs: i64) -> String {
    Local.timestamp_opt(epoch_secs, 0).single().map_or_else(
        || "1970-01-01".to_owned(),
        |t| t.format("%Y-%m-%d").to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_no_part_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        write_new_export(&dest, b"finished").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"finished");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn noclobber_refuses_an_existing_file_and_cleans_its_temp() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        std::fs::write(&dest, b"previous export").unwrap();
        assert!(write_new_export(&dest, b"new pixels").is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous export");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn replace_policy_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        std::fs::write(&dest, b"old").unwrap();
        write_atomic_with_policy(&dest, b"new", true).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
    }

    #[test]
    fn a_failed_write_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("missing-subdir").join("photo.jpg");
        assert!(write_new_export(&dest, b"x").is_err());
        assert!(!dest.exists());
    }

    #[test]
    fn protects_sources_through_dot_aliases_and_allows_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.png");
        std::fs::write(&source, b"original").unwrap();
        let sources = vec![source.to_string_lossy().into_owned()];
        assert!(ensure_not_source(&source, &sources).is_err());
        assert!(ensure_not_source(&dir.path().join("./original.png"), &sources).is_err());
        let up = dir.path().join("sub/../original.png");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert!(ensure_not_source(&up, &sources).is_err());
        assert!(ensure_not_source(&dir.path().join("export.png"), &sources).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn protects_symlink_aliases_and_missing_sources_through_parent_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.png");
        let alias = dir.path().join("alias.png");
        std::fs::write(&source, b"original").unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert!(ensure_not_source(&alias, &[source.to_string_lossy().into_owned()]).is_err());

        let original_dir = dir.path().join("originals");
        let alias_dir = dir.path().join("aliasdir");
        std::fs::create_dir(&original_dir).unwrap();
        std::os::unix::fs::symlink(&original_dir, &alias_dir).unwrap();
        let missing = [original_dir
            .join("missing.dng")
            .to_string_lossy()
            .into_owned()];
        assert!(ensure_not_source(&alias_dir.join("missing.dng"), &missing).is_err());
    }

    #[test]
    fn local_ymd_formats_a_day() {
        let day = local_ymd(1_780_000_000);
        assert_eq!(day.len(), 10);
        assert_eq!(&day[4..5], "-");
    }
}
