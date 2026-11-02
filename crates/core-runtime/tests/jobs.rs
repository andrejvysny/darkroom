use core_app::jobs::{JobClass, JobFailure, JobOwner, JobSpec, QueuePolicy, SubmitError};
use core_model::identity::CatalogEpoch;
use core_runtime::jobs::{Config, Supervisor};
use std::sync::{mpsc, Arc};

fn spec(class: JobClass, policy: QueuePolicy) -> JobSpec {
    JobSpec {
        owner: JobOwner {
            catalog_epoch: CatalogEpoch(1),
            editor_session: None,
        },
        class,
        policy,
        estimated_bytes: 10,
        durable: false,
    }
}
fn supervisor() -> Supervisor {
    Supervisor::new(
        Config {
            max_jobs: 8,
            max_bytes: 80,
            max_terminal_records: 16,
            workers: 2,
        },
        Arc::new(|| {}),
    )
    .unwrap()
}
#[test]
fn dedup_and_panic_leave_worker_usable() {
    let pool = supervisor();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Deduplicated { key: 1 }),
            move |_| {
                started_tx.send(()).unwrap();
                finish_rx.recv().unwrap();
                Ok(4)
            },
        )
        .unwrap();
    started_rx.recv().unwrap();
    assert!(
        matches!(pool.submit(spec(JobClass::Analysis, QueuePolicy::Deduplicated { key: 1 }), |_| Ok(5)), Err(SubmitError::Duplicate(id)) if id == job.id())
    );
    finish_tx.send(()).unwrap();
    assert_eq!(job.wait(), Ok(4));
    let panicked = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered),
            |_| -> Result<(), JobFailure> { panic!("test") },
        )
        .unwrap();
    assert_eq!(panicked.wait(), Err(JobFailure::Panic));
    let next = pool
        .submit(spec(JobClass::Analysis, QueuePolicy::Ordered), |_| Ok(7))
        .unwrap();
    assert_eq!(next.wait(), Ok(7));
    pool.join();
}
#[test]
fn durable_job_drains_on_shutdown() {
    let pool = supervisor();
    let mut durable = spec(JobClass::Export, QueuePolicy::Ordered);
    durable.durable = true;
    let job = pool.submit(durable, |_| Ok(42)).unwrap();
    pool.join();
    assert_eq!(job.wait(), Ok(42));
    assert_eq!(pool.metrics().reserved_bytes, 0);
}

#[test]
fn latest_reclaims_queued_budget_and_keeps_old_handles_readable() {
    let pool = Supervisor::new(
        Config {
            max_jobs: 2,
            max_bytes: 20,
            max_terminal_records: 2,
            workers: 2,
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let blocker = pool
        .submit(spec(JobClass::Analysis, QueuePolicy::Ordered), move |_| {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
    started_rx.recv().unwrap();
    let mut previous = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Latest { key: 5 }),
            |_| Ok(0),
        )
        .unwrap();
    for i in 1..20 {
        let next = pool
            .submit(
                spec(JobClass::Analysis, QueuePolicy::Latest { key: 5 }),
                move |_| Ok(i),
            )
            .unwrap();
        assert_eq!(previous.wait(), Err(JobFailure::Superseded));
        assert_eq!(previous.wait(), Err(JobFailure::Consumed));
        previous = next;
    }
    finish_tx.send(()).unwrap();
    assert_eq!(blocker.wait(), Ok(()));
    assert_eq!(previous.wait(), Ok(19));
}

#[test]
fn cancelled_task_drop_can_reenter_supervisor() {
    struct Reenter(Arc<Supervisor>);
    impl Drop for Reenter {
        fn drop(&mut self) {
            let _ = self.0.metrics();
        }
    }
    let pool = Arc::new(supervisor());
    let (started_tx, started_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let blocker = pool
        .submit(spec(JobClass::Analysis, QueuePolicy::Ordered), move |_| {
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
    started_rx.recv().unwrap();
    let guard = Reenter(pool.clone());
    let pending = pool
        .submit(spec(JobClass::Analysis, QueuePolicy::Ordered), move |_| {
            let _guard = guard;
            Ok(())
        })
        .unwrap();
    assert!(pending.cancel());
    assert_eq!(pending.wait(), Err(JobFailure::Cancelled));
    finish_tx.send(()).unwrap();
    assert_eq!(blocker.wait(), Ok(()));
}
