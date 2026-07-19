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

/// Ordered migration SQL. `to_latest` sets `PRAGMA user_version` to the count applied, so the slice
/// length is also the latest schema version (see [`LATEST_SCHEMA_VERSION`]). Append new files here.
const MIGRATION_SQL: &[&str] = &[
    include_str!("../migrations/001_init.sql"),
    include_str!("../migrations/002_keyword_unique.sql"),
    include_str!("../migrations/003_scale.sql"),
    include_str!("../migrations/004_phash.sql"),
    include_str!("../migrations/005_analysis.sql"),
    include_str!("../migrations/006_labels.sql"),
    include_str!("../migrations/007_user_events.sql"),
    include_str!("../migrations/008_presence.sql"),
    include_str!("../migrations/009_integrity.sql"),
    include_str!("../migrations/010_faces.sql"),
    include_str!("../migrations/011_imported_index.sql"),
    include_str!("../migrations/012_analysis_stage_index.sql"),
    include_str!("../migrations/013_face_marker_reconcile.sql"),
    include_str!("../migrations/014_similarity_features.sql"),
    include_str!("../migrations/015_presets.sql"),
    include_str!("../migrations/016_develop_snapshots.sql"),
    include_str!("../migrations/017_image_format.sql"),
    include_str!("../migrations/018_panorama.sql"),
    include_str!("../migrations/019_pano_detect.sql"),
];

/// Highest schema version this build understands (= number of migrations). A catalog whose
/// `user_version` exceeds this was written by a newer build and is refused (downgrade guard).
const LATEST_SCHEMA_VERSION: i64 = MIGRATION_SQL.len() as i64;

static MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::new(MIGRATION_SQL.iter().map(|s| M::up(s)).collect()));

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
        // Integrity gate: fail loudly on a corrupt catalog (so the app can offer to rebuild from
        // sidecars) instead of silently operating on damaged data. `quick_check` is the fast
        // structural pass; a healthy (or empty/new) DB returns the single row "ok".
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(DbError::Corrupt(check));
        }
        // Downgrade guard: refuse a catalog written by a newer build — running newer schema on older
        // code (or "migrating down") would corrupt it.
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if user_version > LATEST_SCHEMA_VERSION {
            return Err(DbError::SchemaTooNew {
                found: user_version,
                supported: LATEST_SCHEMA_VERSION,
            });
        }
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
        let wiped = (|| {
            let tx = self.conn.transaction()?;
            for t in &tables {
                tx.execute(&format!("DELETE FROM \"{t}\""), [])?;
            }
            tx.commit()
        })();
        // ALWAYS restore enforcement, even if the wipe errored mid-way. This is the single
        // process-wide connection (see `state.rs`), so leaving `foreign_keys` OFF would silently
        // break every later cascade delete (dedup resolve, keyword/collection delete) for the rest
        // of the session — orphaned child rows with no surfaced error.
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        wiped?;
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
    fn migration_017_adds_format_column() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(LATEST_SCHEMA_VERSION, 19, "expected 19 migrations");
        let has_format: bool = db
            .conn
            .prepare("SELECT 1 FROM pragma_table_info('images') WHERE name = 'format'")
            .unwrap()
            .exists([])
            .unwrap();
        assert!(
            has_format,
            "images.format column missing after migration 017"
        );
    }

    /// Inserts a folder + an image row (owned by that folder) and returns the image id. Mirrors the
    /// minimal-columns insert helper style used by the `core-library` integration tests (only the
    /// `NOT NULL` columns are set; everything else takes its default/NULL).
    fn insert_folder_and_image(conn: &Connection, tag: u8) -> i64 {
        conn.execute(
            "INSERT INTO folders(path, added_at) VALUES (?1, 0)",
            rusqlite::params![format!("/lib/folder{tag}")],
        )
        .unwrap();
        let folder_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, folder_id, original_filename, status, imported_at)
             VALUES (?1, 1, ?2, ?3, ?4, 'present', 0)",
            rusqlite::params![vec![tag; 32], format!("/lib/{tag}.cr3"), folder_id, format!("{tag}.cr3")],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    #[test]
    fn migration_018_adds_panorama_sources() {
        let db = Db::open_in_memory().unwrap();

        // (a) Deleting the pano (parent) image cascades to the link row.
        let pano1 = insert_folder_and_image(&db.conn, 1);
        let src1 = insert_folder_and_image(&db.conn, 2);
        db.conn
            .execute(
                "INSERT INTO panorama_sources(pano_image_id, source_image_id, position) VALUES (?1, ?2, 0)",
                rusqlite::params![pano1, src1],
            )
            .unwrap();
        db.conn
            .execute("DELETE FROM images WHERE id = ?1", rusqlite::params![pano1])
            .unwrap();
        let links: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM panorama_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            links, 0,
            "deleting the pano image must cascade-delete its panorama_sources link row"
        );

        // (b) Deleting a source image cascades to the link row but leaves the pano image intact.
        let pano2 = insert_folder_and_image(&db.conn, 3);
        let src2 = insert_folder_and_image(&db.conn, 4);
        db.conn
            .execute(
                "INSERT INTO panorama_sources(pano_image_id, source_image_id, position) VALUES (?1, ?2, 0)",
                rusqlite::params![pano2, src2],
            )
            .unwrap();
        db.conn
            .execute("DELETE FROM images WHERE id = ?1", rusqlite::params![src2])
            .unwrap();
        let links2: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM panorama_sources", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            links2, 0,
            "deleting a source image must cascade-delete its panorama_sources link row"
        );
        let pano_still_present: bool = db
            .conn
            .prepare("SELECT 1 FROM images WHERE id = ?1")
            .unwrap()
            .exists(rusqlite::params![pano2])
            .unwrap();
        assert!(
            pano_still_present,
            "the pano image itself must survive its source frame's deletion"
        );
    }

    #[test]
    fn migration_019_pano_detect_cascades() {
        let db = Db::open_in_memory().unwrap();

        let image_a = insert_folder_and_image(&db.conn, 1);
        let image_b = insert_folder_and_image(&db.conn, 2);

        db.conn
            .execute(
                "INSERT INTO pano_detect_groups(member_key, algo_version, confidence, detected_at, merged_image_id)
                 VALUES ('k1', 'v1', 1.5, 0, ?1)",
                rusqlite::params![image_b],
            )
            .unwrap();
        let group_id = db.conn.last_insert_rowid();
        db.conn
            .execute(
                "INSERT INTO pano_detect_members(group_id, image_id, position) VALUES (?1, ?2, 0)",
                rusqlite::params![group_id, image_a],
            )
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO pano_detect_scan(image_id, algo_version, scanned_at) VALUES (?1, 'v1', 0)",
                rusqlite::params![image_a],
            )
            .unwrap();

        // (a) Deleting a member image cascades both its membership row and its scan marker.
        db.conn
            .execute("DELETE FROM images WHERE id = ?1", rusqlite::params![image_a])
            .unwrap();
        let members: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pano_detect_members", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            members, 0,
            "deleting a member image must cascade-delete its pano_detect_members row"
        );
        let scans: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM pano_detect_scan", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            scans, 0,
            "deleting a member image must cascade-delete its pano_detect_scan row"
        );

        // (b) Deleting the merged image sets merged_image_id NULL but leaves the group row intact.
        db.conn
            .execute("DELETE FROM images WHERE id = ?1", rusqlite::params![image_b])
            .unwrap();
        let merged_image_id: Option<i64> = db
            .conn
            .query_row(
                "SELECT merged_image_id FROM pano_detect_groups WHERE id = ?1",
                rusqlite::params![group_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            merged_image_id, None,
            "deleting the merged image must null out pano_detect_groups.merged_image_id"
        );

        // (c) member_key is UNIQUE — a second group with the same key must fail to insert.
        let dup = db.conn.execute(
            "INSERT INTO pano_detect_groups(member_key, algo_version, confidence, detected_at) VALUES ('k1', 'v2', 0.5, 1)",
            [],
        );
        assert!(
            dup.is_err(),
            "inserting a second group with a duplicate member_key must violate the UNIQUE constraint"
        );
    }

    #[test]
    fn opens_and_creates_all_tables() {
        let db = Db::open_in_memory().unwrap();
        let mut names: Vec<String> = db
            .conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        names.sort();
        // Exact catalog table set across migrations 001–008 (rusqlite_migration tracks its version
        // via PRAGMA user_version, adding no table). Asserting the explicit set — not a loose count —
        // so a dropped/renamed/forgotten table in a future migration fails the test.
        let mut expected = vec![
            "analysis_results",
            "app_meta",
            "collection_images",
            "collections",
            "develop_snapshots",
            "edits",
            "face",
            "face_embedding",
            "face_rejection",
            "folders",
            "image_captions",
            "image_detections",
            "image_features",
            "image_keywords",
            "image_presence",
            "image_similarity_features",
            "image_user_labels",
            "images",
            "import_sessions",
            "keywords",
            "pano_detect_groups",
            "pano_detect_members",
            "pano_detect_scan",
            "panorama_sources",
            "person",
            "presets",
            "ratings_flags",
            "user_events",
        ];
        expected.sort();
        assert_eq!(names, expected, "catalog table set drifted from migrations");
    }

    #[test]
    fn refuses_newer_schema() {
        let path =
            std::env::temp_dir().join(format!("darkroom_schema_guard_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // Create + migrate to latest normally.
        Db::open(&path).unwrap();
        // Simulate a catalog written by a newer build (user_version beyond what we support).
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&format!(
                "PRAGMA user_version = {};",
                LATEST_SCHEMA_VERSION + 1
            ))
            .unwrap();
        }
        assert!(
            matches!(Db::open(&path), Err(DbError::SchemaTooNew { .. })),
            "must refuse a catalog from a newer build"
        );
        for ext in ["db", "db-wal", "db-shm"] {
            let _ = std::fs::remove_file(path.with_extension(ext));
        }
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
