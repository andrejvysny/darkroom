//! Catalog backup: a `VACUUM INTO` snapshot to a timestamped file, with rotation to keep only
//! the newest `keep` copies. `VACUUM INTO` also compacts (no WAL/shm alongside it), so a backup
//! doubles as a clean single-file copy of the catalog.

use crate::DbError;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const PREFIX: &str = "catalog-";
/// Pre-upgrade safety snapshots (see [`backup_pre_upgrade`]). Shares the [`PREFIX`] spelling but is
/// deliberately excluded from rotation and from [`backups`].
const PRE_PREFIX: &str = "catalog-pre-";
const SUFFIX: &str = ".db";

/// Snapshot `conn` into `<backups_dir>/catalog-<UTC stamp>.db`, then delete backups beyond the
/// newest `keep`. Returns the new file's path.
pub fn backup_now(conn: &Connection, backups_dir: &Path, keep: usize) -> Result<PathBuf, DbError> {
    std::fs::create_dir_all(backups_dir)?;

    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let dest = backups_dir.join(format!("{PREFIX}{stamp}{SUFFIX}"));
    vacuum_into(conn, &dest)?;
    rotate(backups_dir, keep)?;
    Ok(dest)
}

/// Snapshot `conn` into `<backups_dir>/catalog-pre-v{version}-<UTC stamp>.db` and return its path.
///
/// The safety copy taken BEFORE a schema upgrade (see `Db::open_with_upgrade_backup`). Unlike
/// rotated backups it is never deleted: the pre-upgrade copy is the only way back if a migration
/// turns out to be wrong, and rotating it away on the next manual backup would throw that away. It
/// is likewise invisible to [`backups`] — it is not a restore point the UI can offer (opening it
/// would just re-run the same migrations) and must not be mistaken for today's daily backup.
pub fn backup_pre_upgrade(
    conn: &Connection,
    backups_dir: &Path,
    version: i64,
) -> Result<PathBuf, DbError> {
    std::fs::create_dir_all(backups_dir)?;
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let dest = backups_dir.join(format!("{PRE_PREFIX}v{version}-{stamp}{SUFFIX}"));
    vacuum_into(conn, &dest)?;
    Ok(dest)
}

/// `VACUUM INTO` a single-file snapshot at `dest`.
fn vacuum_into(conn: &Connection, dest: &Path) -> Result<(), DbError> {
    // Vacuum into a sibling `.part` and rename on success: a crash or a full disk mid-vacuum must
    // not leave a truncated file under a real backup name, which rotation would then keep as if it
    // were the good copy. `VACUUM INTO` refuses to write to an existing target file — that is why
    // the pre-removal below exists at all, and why the temp path must be cleared BEFORE the vacuum
    // rather than after. `dest` is cleared for the same reason: two backups within the same second
    // would otherwise collide (rare — a manual double-click), and the vacuum runs before the rename.
    let part = dest.with_extension("db.part");
    if part.exists() {
        std::fs::remove_file(&part)?;
    }
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }

    let part_str = part.to_str().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "backup path is not valid UTF-8",
        )
    })?;
    // VACUUM INTO takes a string literal, not a bound parameter — escape embedded single quotes
    // (standard SQL string-literal doubling) so a stray `'` in the path can't break out of it.
    let escaped = part_str.replace('\'', "''");
    if let Err(e) = conn.execute_batch(&format!("VACUUM INTO '{escaped}'")) {
        let _ = std::fs::remove_file(&part);
        return Err(e.into());
    }
    std::fs::rename(&part, dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&part);
    })?;
    Ok(())
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
    files.sort_by_key(|file| std::cmp::Reverse(file.1));
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
            // `catalog-pre-*` starts with `catalog-`, so it must be excluded explicitly: a
            // pre-upgrade snapshot is neither rotatable nor a restorable daily copy.
            .is_some_and(|n| {
                n.starts_with(PREFIX) && !n.starts_with(PRE_PREFIX) && n.ends_with(SUFFIX)
            })
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
    fn independent_read_only_connection_can_back_up_a_live_wal_catalog() {
        let root = std::env::temp_dir().join(format!("dr_read_backup_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("catalog.db");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE test(value INTEGER); INSERT INTO test VALUES(7);").unwrap();
        let reader =
            Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let backup = backup_now(&reader, &root.join("backups"), 1).unwrap();
        let check = Connection::open(backup).unwrap();
        assert_eq!(
            check
                .query_row("SELECT value FROM test", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            7
        );
        drop(check);
        drop(reader);
        drop(writer);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backups_of_missing_dir_is_empty() {
        let missing =
            std::env::temp_dir().join(format!("dr_core_db_backup_missing_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(backups(&missing).is_empty());
    }
}
