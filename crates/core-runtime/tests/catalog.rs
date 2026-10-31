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
fn open_classifies_errors_and_clears_previous_catalog() {
    let fixture = Fixture::new();
    let adapter = CatalogAdapter::default();
    assert_eq!(adapter.image(1), Err(AppError::NotOpen));
    adapter.open(&fixture.0.join("copied.db")).unwrap();
    assert_eq!(
        adapter.open(&fixture.0.join("newer.db")),
        Err(AppError::SchemaTooNew {
            found: 26,
            supported: 25
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
