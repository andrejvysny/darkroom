//! Pre-upgrade safety snapshot (`Db::open_with_upgrade_backup`).
//!
//! Migrations have no down path, so the snapshot taken before rewriting an existing catalog is the
//! only way back from a bad migration. These tests pin the three things that matter: the snapshot is
//! the catalog AS IT WAS (old version, old rows), it is taken before anything is rewritten and
//! therefore survives, and its absence is fatal — an upgrade never runs without it.

use core_db::rusqlite::{params, Connection};
use core_db::{backup, Db, DbError};
use rusqlite_migration::{Migrations, M};
use std::path::{Path, PathBuf};

/// The first 25 migrations — i.e. the schema as it was before the sidecar outbox (026). Applied via
/// the same engine `Db::open` uses, so `user_version` lands on 25 exactly as it did in the field.
const MIGRATIONS_V25: &[&str] = &[
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
    include_str!("../migrations/020_hdr_sources.sql"),
    include_str!("../migrations/021_image_pairs.sql"),
    include_str!("../migrations/022_image_stage_attempt.sql"),
    include_str!("../migrations/023_clip_embedding.sql"),
    include_str!("../migrations/024_suggestions.sql"),
    include_str!("../migrations/025_decode_failure.sql"),
];

/// A scratch directory per test invocation (tests share one process, so the pid alone is not
/// unique), removed on `Drop` so a failing assertion leaves no litter behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "dr_upgrade_backup_{tag}_{}_{}",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn table_exists(conn: &Connection, name: &str) -> bool {
    conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")
        .unwrap()
        .exists(params![name])
        .unwrap()
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    conn.prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")
        .unwrap()
        .exists(params![table, column])
        .unwrap()
}

/// Build a catalog at exactly schema 25 with one image and one edit row — the state a real user's
/// `catalog.db` is in when a build carrying migration 026 first opens it.
fn create_v25_catalog(path: &Path) -> i64 {
    let mut conn = Connection::open(path).unwrap();
    let migrations = Migrations::new(MIGRATIONS_V25.iter().map(|s| M::up(s)).collect());
    migrations.to_latest(&mut conn).unwrap();
    assert_eq!(
        user_version(&conn),
        25,
        "fixture must be a schema-25 catalog"
    );

    conn.execute(
        "INSERT INTO folders(path, added_at) VALUES (?1, 0)",
        params!["/lib/folder"],
    )
    .unwrap();
    let folder_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, folder_id, original_filename, status, imported_at)
         VALUES (?1, 1, ?2, ?3, ?4, 'present', 0)",
        params![vec![0xabu8; 32], "/lib/folder/shot.cr3", folder_id, "shot.cr3"],
    )
    .unwrap();
    let image_id = conn.last_insert_rowid();
    // v25 `edits` has no revision columns — this row must arrive at revision 0 on upgrade.
    conn.execute(
        "INSERT INTO edits(image_id, process_version, params, updated_at) VALUES (?1, 5, ?2, 100)",
        params![image_id, r#"{"exposure":1.5}"#],
    )
    .unwrap();
    drop(conn);
    image_id
}

#[test]
fn upgrading_a_v25_catalog_snapshots_it_before_migrating() {
    let scratch = Scratch::new("upgrade");
    let catalog = scratch.path("catalog.db");
    let backups_dir = scratch.path("backups");
    let image_id = create_v25_catalog(&catalog);

    let (db, snapshot) = Db::open_with_upgrade_backup(&catalog, &backups_dir).unwrap();
    let snapshot = snapshot.expect("an upgrade must snapshot the catalog it is about to rewrite");

    // Named after the version it upgrades TO, so the set of pre-upgrade copies is self-describing.
    let name = snapshot.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        name.starts_with("catalog-pre-v26-") && name.ends_with(".db"),
        "unexpected pre-upgrade backup name: {name}"
    );
    assert!(snapshot.is_file(), "the snapshot must exist on disk");

    // The snapshot is the catalog AS IT WAS: still schema 25, still holding the pre-upgrade edit,
    // and none of the 026 objects. Proves the copy predates the migration, not just that it exists.
    let copied = Connection::open(&snapshot).unwrap();
    assert_eq!(user_version(&copied), 25);
    assert!(!table_exists(&copied, "sidecar_outbox"));
    assert!(!column_exists(&copied, "edits", "revision"));
    let params_json: String = copied
        .query_row(
            "SELECT params FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(params_json, r#"{"exposure":1.5}"#);
    drop(copied);

    // The migrated catalog: current schema, 026 objects present, existing row at revision 0.
    let conn = &db.conn;
    assert_eq!(user_version(conn), 26);
    assert!(table_exists(conn, "sidecar_outbox"));
    assert!(column_exists(conn, "edits", "revision"));
    assert!(column_exists(conn, "edits", "mirrored_revision"));
    let (revision, mirrored): (i64, i64) = conn
        .query_row(
            "SELECT revision, mirrored_revision FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (revision, mirrored),
        (0, 0),
        "a pre-existing sidecar is assumed in sync, so a migrated row starts at 0/0"
    );
    let queued: i64 = conn
        .query_row("SELECT COUNT(*) FROM sidecar_outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        queued, 0,
        "migration 026 must queue nothing for existing edits"
    );
    drop(db);

    // A pre-upgrade copy is not a rotated backup: it must survive both the daily rotation and the
    // listing the app uses to decide "when was the last backup".
    let live = Connection::open(&catalog).unwrap();
    let daily = backup::backup_now(&live, &backups_dir, 1).unwrap();
    assert!(
        snapshot.is_file(),
        "rotation must never delete a pre-upgrade snapshot"
    );
    let listed = backup::backups(&backups_dir);
    assert_eq!(
        listed.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        vec![&daily],
        "the pre-upgrade snapshot is not a restorable daily backup"
    );
}

#[test]
fn no_backup_for_a_new_or_already_current_catalog() {
    let scratch = Scratch::new("current");
    let fresh = scratch.path("fresh.db");
    let backups_dir = scratch.path("backups");

    // A brand-new catalog has nothing to lose.
    let (db, snapshot) = Db::open_with_upgrade_backup(&fresh, &backups_dir).unwrap();
    assert!(snapshot.is_none(), "a created catalog needs no snapshot");
    assert_eq!(user_version(&db.conn), 26);
    drop(db);

    // An already-current catalog is not rewritten, so it gets no snapshot either.
    let (db, snapshot) = Db::open_with_upgrade_backup(&fresh, &backups_dir).unwrap();
    assert!(
        snapshot.is_none(),
        "reopening a schema-26 catalog must not create a backup"
    );
    assert!(
        backup::backups(&backups_dir).is_empty(),
        "no upgrade means no files in the backups dir"
    );
    drop(db);
    assert!(
        !backups_dir.exists(),
        "an upgrade-free open must not even create the backups dir"
    );
}

#[test]
fn an_unwritable_backup_dir_aborts_the_upgrade_and_leaves_the_catalog_alone() {
    let scratch = Scratch::new("blocked");
    let catalog = scratch.path("catalog.db");
    let image_id = create_v25_catalog(&catalog);
    // A regular file standing where the backups directory should be: `create_dir_all` cannot
    // succeed whether the tests run as root or not.
    let blocker = scratch.path("blocked");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let backups_dir = blocker.join("backups");

    let err = Db::open_with_upgrade_backup(&catalog, &backups_dir)
        .err()
        .expect("a snapshot that cannot be written must abort the open");
    assert!(
        matches!(err, DbError::Io(_) | DbError::Sqlite(_)),
        "expected an io/sqlite failure, got {err:?}"
    );

    // Never migrated without the backup: the catalog is still schema 25, without the 026 objects,
    // with its edit row intact.
    let conn = Connection::open(&catalog).unwrap();
    assert_eq!(
        user_version(&conn),
        25,
        "a failed snapshot must not migrate"
    );
    assert!(!table_exists(&conn, "sidecar_outbox"));
    let params_json: String = conn
        .query_row(
            "SELECT params FROM edits WHERE image_id = ?1",
            params![image_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(params_json, r#"{"exposure":1.5}"#);
    drop(conn);

    // With the path unblocked, the same catalog upgrades normally.
    std::fs::remove_file(&blocker).unwrap();
    let (_db, snapshot) = Db::open_with_upgrade_backup(&catalog, &backups_dir).unwrap();
    assert!(
        snapshot.is_some(),
        "the retry after fixing the dir must work"
    );
}

#[test]
fn a_newer_schema_is_still_refused_without_touching_anything() {
    let scratch = Scratch::new("too_new");
    let catalog = scratch.path("catalog.db");
    let backups_dir = scratch.path("backups");
    create_v25_catalog(&catalog);
    {
        let conn = Connection::open(&catalog).unwrap();
        conn.execute_batch("PRAGMA user_version = 99;").unwrap();
    }

    let err = Db::open_with_upgrade_backup(&catalog, &backups_dir)
        .err()
        .expect("a catalog from a newer build must be refused");
    assert!(
        matches!(
            err,
            DbError::SchemaTooNew {
                found: 99,
                supported: 26
            }
        ),
        "expected SchemaTooNew, got {err:?}"
    );
    assert!(
        backup::backups(&backups_dir).is_empty() && !backups_dir.exists(),
        "a refused catalog is not an upgrade: no snapshot, no backups dir"
    );

    // Same refusal through the plain entry point.
    assert!(matches!(
        Db::open(&catalog),
        Err(DbError::SchemaTooNew { .. })
    ));
}
