//! Catalog backup: a `VACUUM INTO` snapshot to a timestamped file, with rotation to keep only
//! the newest `keep` copies. `VACUUM INTO` also compacts (no WAL/shm alongside it), so a backup
//! doubles as a clean single-file copy of the catalog.

use crate::DbError;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const PREFIX: &str = "catalog-";
const SUFFIX: &str = ".db";

/// Snapshot `conn` into `<backups_dir>/catalog-<UTC stamp>.db`, then delete backups beyond the
/// newest `keep`. Returns the new file's path.
pub fn backup_now(conn: &Connection, backups_dir: &Path, keep: usize) -> Result<PathBuf, DbError> {
    std::fs::create_dir_all(backups_dir)?;

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let dest = backups_dir.join(format!("{PREFIX}{stamp}{SUFFIX}"));
    // Two backups within the same second would otherwise collide: VACUUM INTO refuses to
    // overwrite an existing target file. Rare (manual double-click), but cheap to guard.
    if dest.exists() {
        std::fs::remove_file(&dest)?;
    }

    let dest_str = dest.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup path is not valid UTF-8",
        )
    })?;
    // VACUUM INTO takes a string literal, not a bound parameter — escape embedded single quotes
    // (standard SQL string-literal doubling) so a stray `'` in the path can't break out of it.
    let escaped = dest_str.replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))?;

    rotate(backups_dir, keep)?;
    Ok(dest)
}

/// Existing backups under `backups_dir`, newest first by mtime. Empty if the directory is
/// missing (never backed up yet) rather than an error.
pub fn backups(backups_dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(backups_dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| is_backup_file(p))
        .filter_map(|p| {
            let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            Some((p, mtime))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files
}

/// Delete backups beyond the newest `keep`. Sorted by filename (descending) rather than mtime —
/// the fixed-width timestamp format sorts identically, and this stays correct even if a copy's
/// mtime is touched (e.g. by a file-sync tool) after it was written.
fn rotate(backups_dir: &Path, keep: usize) -> Result<(), DbError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(backups_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| is_backup_file(p))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for stale in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(stale);
    }
    Ok(())
}

fn is_backup_file(p: &Path) -> bool {
    p.is_file()
        && p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(PREFIX) && n.ends_with(SUFFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_now_rotates_and_produces_a_valid_db() {
        let root = std::env::temp_dir().join(format!("dr_core_db_backup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let conn = Connection::open(root.join("source.db")).unwrap();
        conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();

        let backups_dir = root.join("backups");
        backup_now(&conn, &backups_dir, 1).unwrap();
        // Force a distinct timestamp (second-resolution stamp) so the second call doesn't just
        // collide with the first's filename — this is what actually exercises rotation.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let second = backup_now(&conn, &backups_dir, 1).unwrap();

        let kept = backups(&backups_dir);
        assert_eq!(kept.len(), 1, "keep=1 must leave exactly one backup file");
        assert_eq!(kept[0].0, second);

        // The remaining file opens as a valid, intact sqlite db.
        let check = Connection::open(&second).unwrap();
        let count: i64 = check
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn backups_of_missing_dir_is_empty() {
        let missing =
            std::env::temp_dir().join(format!("dr_core_db_backup_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(backups(&missing).is_empty());
    }
}
