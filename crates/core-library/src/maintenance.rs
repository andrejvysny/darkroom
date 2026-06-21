//! Catalog maintenance / crash-recovery helpers run at app startup.

use crate::error::LibError;
use crate::index::now_epoch;
use core_db::rusqlite::Connection;

/// Stamp `finished_at` on any import sessions left open by a previous run that crashed or was killed
/// mid-import (the normal path finishes the session on exit). Returns the number reaped. The library
/// copies of those imports are already durably catalogued per-file, so this only closes the dangling
/// session bookkeeping (otherwise `finished_at IS NULL` rows accumulate forever).
pub fn reap_dangling_import_sessions(conn: &Connection) -> Result<usize, LibError> {
    let n = conn.execute(
        "UPDATE import_sessions SET finished_at = ?1 WHERE finished_at IS NULL",
        [now_epoch()],
    )?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_db::rusqlite::params;
    use core_db::Db;

    #[test]
    fn reaps_only_open_sessions() {
        let db = Db::open_in_memory().unwrap();
        let conn = &db.conn;
        // One dangling (finished_at NULL), one already finished.
        conn.execute(
            "INSERT INTO import_sessions(source_volume, mode, started_at, finished_at)
             VALUES('/card', 'copy', 100, NULL), ('/card', 'copy', 100, 200)",
            [],
        )
        .unwrap();

        let reaped = reap_dangling_import_sessions(conn).unwrap();
        assert_eq!(reaped, 1, "only the open session should be reaped");

        let open: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM import_sessions WHERE finished_at IS NULL",
                params![],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 0, "no sessions should remain open");

        // Idempotent: a second sweep reaps nothing.
        assert_eq!(reap_dangling_import_sessions(conn).unwrap(), 0);
    }
}
