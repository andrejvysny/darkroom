//! Validated, atomic output shared by merge and export writers.

use crate::RawError;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

/// Reject impossible RGB layouts before allocation, indexing, or invoking an encoder.
pub(crate) fn rgb_len(width: u32, height: u32) -> Result<usize, RawError> {
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .filter(|&n| width != 0 && height != 0 && n <= isize::MAX as usize / size_of::<f32>());
    len.ok_or_else(|| RawError::Decode("invalid or overflowing RGB image dimensions".into()))
}

/// Keep the old destination intact until the complete file has been flushed and synced.
/// Merge outputs use `replace = false`; explicit exports may atomically replace an old file.
/// Each invocation owns its unique sibling `.part`, including cleanup on error or unwind.
pub(crate) fn write_atomically(
    dest: &Path,
    replace: bool,
    encode: impl FnOnce(&mut BufWriter<&mut File>) -> Result<(), RawError>,
) -> Result<(), RawError> {
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::Builder::new()
        .prefix(".darkroom-output-")
        .suffix(".part")
        .tempfile_in(parent)?;
    {
        let mut writer = BufWriter::new(temp.as_file_mut());
        encode(&mut writer)?;
        // rawler's DngWriter::close writes the IFD pointer but does not flush its writer.
        // Dropping an owned BufWriter would silently ignore a final disk-full/I/O failure.
        writer.flush()?;
    }
    temp.as_file().sync_all()?;
    let result = if replace {
        temp.persist(dest)
    } else {
        temp.persist_noclobber(dest)
    };
    result.map(|_| ()).map_err(|e| RawError::Io(e.error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_encode_preserves_destination_and_removes_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.dng");
        std::fs::write(&dest, b"original").unwrap();
        for replace in [false, true] {
            let result = write_atomically(&dest, replace, |writer| {
                writer.write_all(b"partial replacement")?;
                Err(RawError::Decode("injected encode failure".into()))
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read(&dest).unwrap(), b"original");
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn merge_publication_is_no_clobber_even_if_destination_appears_during_encode() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.exr");
        let result = write_atomically(&dest, false, |writer| {
            writer.write_all(b"new merge")?;
            std::fs::write(&dest, b"concurrent photograph")?;
            Ok(())
        });
        assert!(
            matches!(result, Err(RawError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(dest).unwrap(), b"concurrent photograph");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn explicit_export_replaces_only_after_success() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.dng");
        std::fs::write(&dest, b"original").unwrap();
        write_atomically(&dest, true, |writer| {
            writer.write_all(b"complete replacement")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), b"complete replacement");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
