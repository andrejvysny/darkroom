use core_app::jobs::JobState;
use core_app::ports::{CatalogPort, Clock, EffectExecutor, EventSink, RenderBackend};
use core_app::state::ImageTags;
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{
    Command, Completion, Effect, Mutation, MutationReport, Operation, Outcome,
};
use core_app::AppError;
use core_model::catalog::QueryParams;
use core_runtime::async_workflow::AsyncCatalogWorkflow;
use core_runtime::jobs::Config;
use core_runtime::workflow::RuntimePaths;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

struct Stub;
impl Clock for Stub {
    fn now_millis(&self) -> u64 {
        0
    }
}
impl EventSink for Stub {
    fn published(&self, _: &SnapshotUpdate) {}
}
impl RenderBackend for Stub {
    fn available(&self) -> bool {
        false
    }
}

struct Executor {
    started: mpsc::Sender<Operation>,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    panic_first: bool,
    fail_open: Option<AppError>,
}
impl EffectExecutor for Executor {
    fn execute(&self, effect: Effect, _: &dyn CatalogPort) -> Completion {
        self.started.send(effect.operation.clone()).unwrap();
        let gate = self.gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.recv().unwrap();
        }
        if self.panic_first && matches!(effect.operation, Operation::OpenCatalog(_)) {
            panic!("executor failure");
        }
        if let Some(error) = &self.fail_open {
            if matches!(effect.operation, Operation::OpenCatalog(_)) {
                return Completion {
                    token: effect.token,
                    result: Err(error.clone()),
                };
            }
        }
        let result = match &effect.operation {
            Operation::OpenCatalog(_) => Outcome::Opened,
            Operation::CloseCatalog => Outcome::Closed,
            Operation::QueryLibrary(query) => Outcome::Library {
                rows: vec![],
                count: query.min_stars.unwrap_or(0),
            },
            Operation::LoadFolders => Outcome::Folders(vec![]),
            Operation::LoadDates => Outcome::Dates(vec![]),
            Operation::SelectImage { .. } => Outcome::Image(None),
            Operation::LoadEdit { .. } => Outcome::Edit(None),
            Operation::SaveEdit { .. } | Operation::SetCull { .. } => {
                unreachable!("no edits in this test")
            }
            Operation::LoadKeywords => Outcome::Keywords(vec![]),
            Operation::LoadCollections => Outcome::Collections(vec![]),
            Operation::LoadImageTags(image) => Outcome::ImageTags(Box::new(ImageTags {
                image: *image,
                ..Default::default()
            })),
            Operation::LoadRejectSummary(_) => Outcome::RejectSummary(Default::default()),
            Operation::Mutate(_) => Outcome::Mutated(MutationReport::Done),
        };
        Completion {
            token: effect.token,
            result: Ok(result),
        }
    }
}

fn workflow(executor: Arc<dyn EffectExecutor>) -> AsyncCatalogWorkflow {
    AsyncCatalogWorkflow::new(
        RuntimePaths {
            catalog: "initial.db".into(),
        },
        Arc::new(Stub),
        Arc::new(Stub),
        Arc::new(Stub),
        executor,
        Arc::new(|| {}),
        Config {
            max_jobs: 1,
            max_bytes: 16 * 1024 * 1024,
            max_terminal_records: 8,
            workers: 2,
        },
    )
    .unwrap()
}
struct ReleaseGuard(Option<mpsc::Sender<()>>);
impl ReleaseGuard {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for ReleaseGuard {
    fn drop(&mut self) {
        self.release();
    }
}

fn next(rx: &mpsc::Receiver<Operation>) -> Operation {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("worker did not start")
}
fn executor(
    block: bool,
    panic_first: bool,
) -> (Arc<Executor>, mpsc::Receiver<Operation>, mpsc::Sender<()>) {
    let (started, rx) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    (
        Arc::new(Executor {
            started,
            gate: Mutex::new(block.then_some(gate)),
            panic_first,
            fail_open: None,
        }),
        rx,
        release,
    )
}

#[test]
fn stale_open_finishes_before_close_and_never_publishes() {
    let (executor, rx, release) = executor(true, false);
    let mut workflow = workflow(executor);
    let mut release = ReleaseGuard(Some(release));
    workflow.open_initial().unwrap();
    assert!(matches!(next(&rx), Operation::OpenCatalog(_)));
    workflow.dispatch(Command::CloseCatalog).unwrap();
    assert!(!workflow.snapshot().catalog_ready);
    assert!(!workflow.pump().unwrap());
    release.release();
    workflow.wait_idle().unwrap();
    assert_eq!(next(&rx), Operation::CloseCatalog);
    assert!(rx.try_recv().is_err());
    assert!(!workflow.snapshot().catalog_ready);
}

#[test]
fn reopen_aba_executes_only_latest_open_after_old_io() {
    let (executor, rx, release) = executor(true, false);
    let mut workflow = workflow(executor);
    let mut release = ReleaseGuard(Some(release));
    workflow
        .dispatch(Command::OpenCatalog("a.db".into()))
        .unwrap();
    assert!(matches!(next(&rx), Operation::OpenCatalog(_)));
    workflow.dispatch(Command::CloseCatalog).unwrap();
    workflow
        .dispatch(Command::OpenCatalog("a.db".into()))
        .unwrap();
    release.release();
    workflow.wait_idle().unwrap();
    assert_eq!(next(&rx), Operation::OpenCatalog("a.db".into()));
    assert!(workflow.snapshot().catalog_ready);
    assert_eq!(workflow.snapshot().catalog_epoch.0, 3);
}

#[test]
fn query_replacements_coalesce_and_publish_latest() {
    let (executor, rx, _) = executor(false, false);
    let mut workflow = workflow(executor);
    workflow.open_initial().unwrap();
    workflow.wait_idle().unwrap();
    while rx.try_recv().is_ok() {}
    for value in 1..=100 {
        let query = QueryParams {
            min_stars: Some(value),
            ..Default::default()
        };
        workflow
            .dispatch(Command::QueryLibrary(Box::new(query)))
            .unwrap();
    }
    workflow.wait_idle().unwrap();
    assert_eq!(workflow.snapshot().library.count, 100);
    assert_eq!(workflow.snapshot().jobs.active_effect_count, 0);
}

#[test]
fn panicking_worker_publishes_failure_and_workflow_remains_usable() {
    let (executor, _rx, _release) = executor(false, true);
    let mut workflow = workflow(executor);
    workflow.open_initial().unwrap();
    workflow.wait_idle().unwrap();
    assert!(workflow.snapshot().error.is_some());
    workflow.dispatch(Command::RequestShutdown).unwrap();
    workflow.wait_idle().unwrap();
    assert!(workflow.snapshot().error.is_none());
}

#[test]
fn shutdown_drains_close_then_joins() {
    let (executor, rx, release) = executor(true, false);
    let mut workflow = workflow(executor);
    let mut release = ReleaseGuard(Some(release));
    workflow.open_initial().unwrap();
    next(&rx);
    workflow.dispatch(Command::RequestShutdown).unwrap();
    assert!(!workflow.needs_join());
    release.release();
    workflow.wait_idle().unwrap();
    assert_eq!(next(&rx), Operation::CloseCatalog);
    assert!(workflow.needs_join());
    assert!(workflow.join_shutdown());
    assert!(workflow.is_shutdown_complete());
}

#[test]
fn repeated_worker_notifications_coalesce_until_pump() {
    let (executor, rx, release) = executor(true, false);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = wakes.clone();
    let mut workflow = AsyncCatalogWorkflow::new(
        RuntimePaths {
            catalog: "initial.db".into(),
        },
        Arc::new(Stub),
        Arc::new(Stub),
        Arc::new(Stub),
        executor,
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
        Config {
            max_jobs: 1,
            max_bytes: 16 * 1024 * 1024,
            max_terminal_records: 8,
            workers: 2,
        },
    )
    .unwrap();
    let mut release = ReleaseGuard(Some(release));
    workflow.open_initial().unwrap();
    next(&rx);
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    release.release();
    workflow.wait_idle().unwrap();
}

#[test]
fn failed_open_is_failed_job_and_keeps_typed_error() {
    let (started, rx) = mpsc::channel();
    let executor = Arc::new(Executor {
        started,
        gate: Mutex::new(None),
        panic_first: false,
        fail_open: Some(AppError::SchemaTooNew {
            found: 99,
            supported: 1,
        }),
    });
    let mut workflow = workflow(executor);
    workflow.open_initial().unwrap();
    assert!(matches!(next(&rx), Operation::OpenCatalog(_)));
    workflow.wait_idle().unwrap();
    assert_eq!(
        workflow.snapshot().error,
        Some(AppError::SchemaTooNew {
            found: 99,
            supported: 1
        })
    );
    assert!(workflow
        .job_snapshots()
        .iter()
        .any(|job| job.state == JobState::Failed));
    workflow.dispatch(Command::RequestShutdown).unwrap();
    workflow.wait_idle().unwrap();
    assert!(workflow
        .job_snapshots()
        .iter()
        .any(|job| job.state == JobState::Succeeded));
}

#[test]
fn mutation_and_catalog_reads_complete_on_the_ordered_lane() {
    let (executor, _rx, _release) = executor(false, false);
    let mut workflow = workflow(executor);
    workflow.open_initial().unwrap();
    workflow.wait_idle().unwrap();
    for _ in 0..20 {
        workflow.dispatch(Command::LoadKeywords).unwrap();
        workflow.dispatch(Command::LoadCollections).unwrap();
    }
    workflow
        .dispatch(Command::LoadRejectSummary(Box::default()))
        .unwrap();
    workflow
        .dispatch(Command::Mutate(Mutation::DeleteKeyword(1)))
        .unwrap();
    workflow.wait_idle().unwrap();
    let snapshot = workflow.snapshot();
    assert_eq!(snapshot.library.last_report, Some(MutationReport::Done));
    assert_eq!(snapshot.library.tags_revision, 1);
    assert!(snapshot.error.is_none());
}
