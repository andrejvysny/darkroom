#[path = "../../core-library/tests/support/migration_catalog.rs"]
mod migration_catalog;

use core_app::{ports::CatalogPort, AppError};
use core_model::catalog::QueryParams;
use core_runtime::catalog::{open_database, CatalogAdapter};
use std::{
    error::Error,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "darkroom-runtime-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        migration_catalog::generate(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn copied_catalog_matches_library_queries_and_reopens() {
    let fixture = Fixture::new();
    let path = fixture.0.join("copied.db");
    let shared = open_database(&path).unwrap();
    let adapter = CatalogAdapter::from_shared(shared.clone());
    let db = core_db::Db::open(&path).unwrap();
    for params in [
        QueryParams::default(),
        QueryParams {
            folder_id: Some(1),
            ..Default::default()
        },
        QueryParams {
            folder_id: Some(-1),
            ..Default::default()
        },
    ] {
        assert_eq!(
            adapter.query(&params).unwrap(),
            core_library::query_images(&db.conn, &params).unwrap()
        );
        assert_eq!(
            adapter.count(&params).unwrap(),
            core_library::count_images(&db.conn, &params).unwrap()
        );
    }
    assert_eq!(
        adapter.folders().unwrap(),
        core_library::list_folders(&db.conn).unwrap()
    );
    assert_eq!(
        adapter.dates().unwrap(),
        core_library::date_tree(&db.conn).unwrap()
    );
    for id in [-1, 1, 5, 999] {
        assert_eq!(
            adapter.image(id).unwrap(),
            core_library::image_by_id(&db.conn, id).unwrap()
        );
    }
    drop(db);
    adapter.close().unwrap();
    assert_eq!(
        adapter.count(&QueryParams::default()),
        Err(AppError::NotOpen)
    );
    adapter.open(&path).unwrap();
    assert_eq!(adapter.count(&QueryParams::default()).unwrap(), 4);
}

#[test]
fn edit_returns_stored_params_with_their_process_version() {
    let fixture = Fixture::new();
    let path = fixture.0.join("copied.db");
    let adapter = CatalogAdapter::from_shared(open_database(&path).unwrap());
    // The fixture stores a v5 and a v6 edit; images 3/4/5 and unknown ids have no row at all.
    assert_eq!(
        adapter.edit(1).unwrap(),
        Some((migration_catalog::PV5.to_string(), 5))
    );
    assert_eq!(
        adapter.edit(2).unwrap(),
        Some((migration_catalog::PV6.to_string(), 6))
    );
    assert_eq!(adapter.edit(3).unwrap(), None);
    assert_eq!(adapter.edit(999).unwrap(), None);
    let db = core_db::Db::open(&path).unwrap();
    core_library::set_edit(&db.conn, 3, 6, "{\"exposure\":1.0}", 200).unwrap();
    drop(db);
    assert_eq!(
        adapter.edit(3).unwrap(),
        Some(("{\"exposure\":1.0}".to_string(), 6))
    );
    adapter.close().unwrap();
    assert_eq!(adapter.edit(1), Err(AppError::NotOpen));
}

#[test]
fn open_classifies_errors_and_clears_previous_catalog() {
    let fixture = Fixture::new();
    let adapter = CatalogAdapter::default();
    assert_eq!(adapter.image(1), Err(AppError::NotOpen));
    adapter.open(&fixture.0.join("copied.db")).unwrap();
    assert_eq!(
        adapter.open(&fixture.0.join("newer.db")),
        Err(AppError::SchemaTooNew {
            found: core_db::LATEST_SCHEMA_VERSION + 1,
            supported: core_db::LATEST_SCHEMA_VERSION,
        })
    );
    assert_eq!(adapter.image(1), Err(AppError::NotOpen));
    let corrupt = adapter.open(&fixture.0.join("corrupt.db")).unwrap_err();
    assert!(matches!(corrupt, AppError::Corrupt(_)));
    assert!(corrupt
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<core_db::DbError>()
        .is_some());
    assert!(corrupt.to_string().contains("sqlite:"));
    let missing_parent = fixture.0.join("absent/catalog.db");
    let missing = adapter.open(&missing_parent).unwrap_err();
    assert!(matches!(missing, AppError::Catalog(_)));
    assert!(missing
        .source()
        .unwrap()
        .source()
        .unwrap()
        .downcast_ref::<core_db::DbError>()
        .is_some());
    assert!(missing.to_string().contains("sqlite:"));
}

// ---------- pre-upgrade backup on the production open path ----------

/// The first 25 migrations — the schema as it was before the sidecar outbox (026). Same pinned
/// engine and files `Db::open` uses (mirrors `crates/core-db/tests/upgrade_backup.rs`), so
/// `user_version` lands on 25 exactly as it did in the field.
const MIGRATIONS_V25: &[&str] = &[
    include_str!("../../core-db/migrations/001_init.sql"),
    include_str!("../../core-db/migrations/002_keyword_unique.sql"),
    include_str!("../../core-db/migrations/003_scale.sql"),
    include_str!("../../core-db/migrations/004_phash.sql"),
    include_str!("../../core-db/migrations/005_analysis.sql"),
    include_str!("../../core-db/migrations/006_labels.sql"),
    include_str!("../../core-db/migrations/007_user_events.sql"),
    include_str!("../../core-db/migrations/008_presence.sql"),
    include_str!("../../core-db/migrations/009_integrity.sql"),
    include_str!("../../core-db/migrations/010_faces.sql"),
    include_str!("../../core-db/migrations/011_imported_index.sql"),
    include_str!("../../core-db/migrations/012_analysis_stage_index.sql"),
    include_str!("../../core-db/migrations/013_face_marker_reconcile.sql"),
    include_str!("../../core-db/migrations/014_similarity_features.sql"),
    include_str!("../../core-db/migrations/015_presets.sql"),
    include_str!("../../core-db/migrations/016_develop_snapshots.sql"),
    include_str!("../../core-db/migrations/017_image_format.sql"),
    include_str!("../../core-db/migrations/018_panorama.sql"),
    include_str!("../../core-db/migrations/019_pano_detect.sql"),
    include_str!("../../core-db/migrations/020_hdr_sources.sql"),
    include_str!("../../core-db/migrations/021_image_pairs.sql"),
    include_str!("../../core-db/migrations/022_image_stage_attempt.sql"),
    include_str!("../../core-db/migrations/023_clip_embedding.sql"),
    include_str!("../../core-db/migrations/024_suggestions.sql"),
    include_str!("../../core-db/migrations/025_decode_failure.sql"),
];

/// A production-shaped startup: an existing v25 `catalog.db` opened through the adapter host must
/// get a `catalog-pre-v26-*` safety snapshot next to it BEFORE the migration rewrites it. This is
/// the path `AppState::new` and `CatalogPort::open` both take; `Db::open` alone would migrate with
/// no way back.
#[test]
fn opening_a_v25_catalog_through_the_adapter_backs_it_up_before_migrating() {
    let fixture = Fixture::new();
    let path = fixture.0.join("v25.db");
    {
        let mut conn = core_db::rusqlite::Connection::open(&path).unwrap();
        let migrations = rusqlite_migration::Migrations::new(
            MIGRATIONS_V25
                .iter()
                .map(|s| rusqlite_migration::M::up(s))
                .collect(),
        );
        migrations.to_latest(&mut conn).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            25,
            "fixture must be a schema-25 catalog"
        );
        conn.execute(
            "INSERT INTO folders(path, added_at) VALUES ('/lib/folder', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO images(content_hash, file_size, path, folder_id, original_filename,
                               status, imported_at)
             VALUES (?1, 1, '/lib/folder/shot.cr3', 1, 'shot.cr3', 'present', 0)",
            core_db::rusqlite::params![vec![0xabu8; 32]],
        )
        .unwrap();
    }

    let shared = open_database(&path).unwrap();

    // The migrated catalog reached the current schema.
    {
        let db = shared.lock().unwrap();
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            core_db::LATEST_SCHEMA_VERSION,
            "the opened catalog was migrated"
        );
    }

    // The snapshot sits beside the catalog, named after the version it upgrades TO, and is the
    // catalog AS IT WAS: still schema 25, still holding the pre-upgrade row.
    let backups = fixture.0.join("backups");
    let snapshots: Vec<PathBuf> = std::fs::read_dir(&backups)
        .expect("a backups directory beside the catalog")
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(
        snapshots.len(),
        1,
        "exactly one pre-upgrade snapshot: {snapshots:?}"
    );
    let snapshot = &snapshots[0];
    let name = snapshot.file_name().unwrap().to_str().unwrap().to_string();
    assert!(
        name.starts_with("catalog-pre-v26-") && name.ends_with(".db"),
        "unexpected pre-upgrade backup name: {name}"
    );
    let copied = core_db::rusqlite::Connection::open(snapshot).unwrap();
    assert_eq!(
        copied
            .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        25,
        "the snapshot predates the migration"
    );
    assert_eq!(
        copied
            .query_row("SELECT COUNT(*) FROM images", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1,
        "the snapshot holds the pre-upgrade rows"
    );
}

/// Reopening an already-current catalog must not produce another snapshot — only an actual
/// upgrade rewrites anything.
#[test]
fn reopening_a_current_catalog_takes_no_backup() {
    let fixture = Fixture::new();
    let path = fixture.0.join("current.db");
    drop(open_database(&path).unwrap());
    // A brand-new catalog migrated from 0 with nothing to lose; reopen is a no-schema-change.
    assert!(!fixture.0.join("backups").exists() || snapshot_count(&fixture.0) == 0);
    drop(open_database(&path).unwrap());
    assert_eq!(snapshot_count(&fixture.0), 0);
}

fn snapshot_count(dir: &std::path::Path) -> usize {
    let dir = dir.join("backups");
    std::fs::read_dir(&dir)
        .map(|entries| entries.filter(|e| e.is_ok()).count())
        .unwrap_or(0)
}
