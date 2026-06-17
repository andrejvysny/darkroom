//! core-db — SQLite catalog (Rust-owned). Frontend never opens the DB directly.
//!
//! Schema lives in `migrations/001_init.sql` and is applied via `rusqlite_migration`.
//! The app layer wraps [`Db`] in a `Mutex` inside Tauri `State`.

pub mod error;

pub use error::DbError;
// Re-export so downstream crates link the exact same rusqlite (avoids libsqlite3-sys conflicts).
pub use rusqlite;

use rusqlite::Connection;
use rusqlite_migration::{Migrations, M};
use std::path::Path;
use std::sync::LazyLock;

static MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!("../migrations/001_init.sql")),
        M::up(include_str!("../migrations/002_keyword_unique.sql")),
        M::up(include_str!("../migrations/003_scale.sql")),
        M::up(include_str!("../migrations/004_phash.sql")),
        M::up(include_str!("../migrations/005_analysis.sql")),
        M::up(include_str!("../migrations/006_labels.sql")),
        M::up(include_str!("../migrations/007_user_events.sql")),
    ])
});

/// Owns a single SQLite connection to the catalog.
pub struct Db {
    pub conn: Connection,
}

impl Db {
    /// Open (or create) the catalog at `path`, apply connection pragmas, and migrate to latest.
    pub fn open(path: &Path) -> Result<Self, DbError> {
        let mut conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;
             PRAGMA cache_size = -32000;",
        )?;
        MIGRATIONS.to_latest(&mut conn)?;
        Ok(Self { conn })
    }

    /// Delete every row from every catalog table, keeping the schema (and migration `user_version`)
    /// intact, then reclaim the freed pages. Used by the "reset catalog" action — it wipes the
    /// index/metadata/settings only; files on disk are never touched.
    pub fn wipe(&mut self) -> Result<(), DbError> {
        // Table names come from sqlite_master (trusted), not user input — safe to interpolate.
        let tables: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(Result::ok).collect()
        };
        // FKs off so deletion order across referencing tables doesn't matter.
        self.conn.execute_batch("PRAGMA foreign_keys = OFF;")?;
        {
            let tx = self.conn.transaction()?;
            for t in &tables {
                tx.execute(&format!("DELETE FROM \"{t}\""), [])?;
            }
            tx.commit()?;
        }
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // VACUUM cannot run inside a transaction — reclaim pages now the rows are gone.
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }

    /// In-memory catalog for tests.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        MIGRATIONS.to_latest(&mut conn)?;
        Ok(Self { conn })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        assert!(MIGRATIONS.validate().is_ok());
    }

    #[test]
    fn opens_and_creates_all_tables() {
        let db = Db::open_in_memory().unwrap();
        let n: i64 = db
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // 13 catalog tables (rusqlite_migration adds no extra user tables).
        assert!(n >= 13, "expected >=13 tables, got {n}");
    }

    #[test]
    fn foreign_keys_enforced() {
        let db = Db::open_in_memory().unwrap();
        // edits.image_id references images(id); inserting an orphan must fail.
        let r = db.conn.execute(
            "INSERT INTO edits(image_id, process_version, params, updated_at) VALUES (999, 1, '{}', 0)",
            [],
        );
        assert!(r.is_err(), "FK violation should be rejected");
    }
}
