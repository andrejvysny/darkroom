#[path = "../../core-library/tests/support/migration_catalog.rs"]
mod migration_catalog;
use core_app::ports::{CatalogPort, Clock, EffectExecutor, EventSink, RenderBackend};
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{Command, Completion, Effect, Operation};
use core_app::AppError;
use core_model::{catalog::QueryParams, identity::ImageId};
use core_runtime::workflow::{CatalogWorkflow, InlineExecutor, RuntimePaths};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

struct Services;
impl Clock for Services {
    fn now_millis(&self) -> u64 {
        42
    }
}
impl EventSink for Services {
    fn published(&self, _: &SnapshotUpdate) {}
}
impl RenderBackend for Services {
    fn available(&self) -> bool {
        false
    }
}
fn paths() -> RuntimePaths {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "darkroom-workflow-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    migration_catalog::generate(&dir).unwrap();
    RuntimePaths {
        catalog: dir.join("copied.db"),
    }
}
fn workflow(paths: RuntimePaths, executor: Arc<dyn EffectExecutor>) -> CatalogWorkflow {
    CatalogWorkflow::new(
        paths,
        Arc::new(Services),
        Arc::new(Services),
        Arc::new(Services),
        executor,
    )
}
fn remove(path: PathBuf) {
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn complete_headless_workflow_preserves_catalog_and_selected_metadata() {
    let paths = paths();
    let file = paths.catalog.clone();
    let mut app = workflow(paths, Arc::new(InlineExecutor));
    let subscription = app.subscribe();
    app.open_initial().unwrap();
    let first = app.snapshot();
    assert_eq!(first.library.count, 4);
    assert_eq!(first.library.rows.len(), 4);
    assert_eq!(first.jobs.active_effect_count, 0);
    assert!(!first.settings.renderer_available);
    app.dispatch(Command::SelectImage(Some(ImageId(1))))
        .unwrap();
    assert_eq!(
        app.snapshot()
            .editor
            .image
            .as_ref()
            .unwrap()
            .camera_model
            .as_deref(),
        Some("Camera")
    );
    app.dispatch(Command::CloseCatalog).unwrap();
    assert!(app.snapshot().library.rows.is_empty());
    app.open_initial().unwrap();
    assert!(app.snapshot().catalog_epoch.0 > first.catalog_epoch.0);
    app.dispatch(Command::QueryLibrary(Box::new(QueryParams {
        include_paired: Some(true),
        ..Default::default()
    })))
    .unwrap();
    assert_eq!(app.snapshot().library.count, 5);
    assert!(subscription.take_latest().unwrap().snapshot.catalog_ready);
    subscription.close();
    app.dispatch(Command::RequestShutdown).unwrap();
    assert!(!app.snapshot().running);
    assert_eq!(app.snapshot().jobs.active_effect_count, 0);
    assert!(subscription.take_latest().is_none());
    drop(app);
    remove(file);
}

struct FailedQuery;
impl EffectExecutor for FailedQuery {
    fn execute(&self, effect: Effect, catalog: &dyn CatalogPort) -> Completion {
        if matches!(effect.operation, Operation::QueryLibrary(_)) {
            Completion {
                token: effect.token,
                result: Err(AppError::Catalog("query failed".into())),
            }
        } else {
            InlineExecutor.execute(effect, catalog)
        }
    }
}
#[test]
fn failed_effect_does_not_leave_other_initial_requests_pending() {
    let paths = paths();
    let file = paths.catalog.clone();
    let mut app = workflow(paths, Arc::new(FailedQuery));
    assert!(app.open_initial().is_err());
    assert!(app.snapshot().catalog_ready);
    assert!(app.snapshot().library.rows.is_empty());
    assert_eq!(app.snapshot().jobs.active_effect_count, 0);
    app.dispatch(Command::RequestShutdown).unwrap();
    drop(app);
    remove(file);
}
