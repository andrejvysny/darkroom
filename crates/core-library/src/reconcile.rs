//! Reconcile the catalog against the filesystem: flip vanished images to `missing`, and resurrect
//! `missing` rows whose file has reappeared. Used on launch and after debounced FS-watcher events.

use crate::error::LibError;
use core_db::rusqlite::{params, Connection};

/// Counts of what a reconcile pass changed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileStats {
    /// `present` rows whose file is gone → flipped to `missing`.
    pub now_missing: usize,
    /// `missing` rows whose file reappeared → flipped to `present`.
    pub restored: usize,
}

/// Re-stat every catalog row against disk and update its `status`. Returns what changed. The status
/// updates run in one transaction so a fully-unplugged volume doesn't fsync per row.
pub fn reconcile(conn: &Connection) -> Result<ReconcileStats, LibError> {
    let rows: Vec<(i64, String, String)> = {
        let mut stmt = conn.prepare("SELECT id, path, status FROM images")?;
        let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        mapped.collect::<core_db::rusqlite::Result<_>>()?
    };

    let mut stats = ReconcileStats::default();
    let tx = conn.unchecked_transaction()?;
    for (id, path, status) in rows {
        let exists = std::path::Path::new(&path).exists();
        match (status.as_str(), exists) {
            ("present", false) => {
                tx.execute(
                    "UPDATE images SET status='missing' WHERE id=?1",
                    params![id],
                )?;
                stats.now_missing += 1;
            }
            ("missing", true) => {
                tx.execute(
                    "UPDATE images SET status='present' WHERE id=?1",
                    params![id],
                )?;
                stats.restored += 1;
            }
            _ => {}
        }
    }
    tx.commit()?;
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::Db;

    fn insert(db: &Db, path: &str, status: &str) {
        db.conn
            .execute(
                "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
                 VALUES(?1, 1, ?2, ?3, ?4, 0)",
                params![&[0u8; 32][..], path, path, status],
            )
            .unwrap();
    }

    #[test]
    fn flips_missing_and_restores() {
        let db = Db::open_in_memory().unwrap();
        let dir = std::env::temp_dir();
        let present_file = dir.join("darkroom_reconcile_present.tmp");
        std::fs::write(&present_file, b"x").unwrap();
        let gone = dir.join("darkroom_reconcile_gone_nonexistent.tmp");
        let _ = std::fs::remove_file(&gone);

        insert(&db, present_file.to_str().unwrap(), "present"); // unchanged
        insert(&db, gone.to_str().unwrap(), "present"); // -> missing
        insert(&db, present_file.to_str().unwrap(), "missing"); // -> restored

        let stats = reconcile(&db.conn).unwrap();
        assert_eq!(stats.now_missing, 1);
        assert_eq!(stats.restored, 1);

        let _ = std::fs::remove_file(&present_file);
    }
}
