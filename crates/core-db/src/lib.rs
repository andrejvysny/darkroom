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
        // 10 catalog tables (rusqlite_migration adds no extra user tables).
        assert!(n >= 10, "expected >=10 tables, got {n}");
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
