use core_runtime::workers::{ManagedWorkers, WorkerState};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

#[test]
fn adapters_are_bounded_joined_and_stop_cannot_be_cleared_by_phase_reset() {
    let workers = ManagedWorkers::new(1);
    let (started, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    workers
        .spawn("producer", move || {
            started.send(()).unwrap();
            proceed.recv().unwrap();
            Ok(())
        })
        .unwrap();
    observed.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(workers.spawn("overflow", || Ok(())).is_err());
    let cancelled = AtomicBool::new(false);
    workers.stop_producers();
    cancelled.store(true, Ordering::SeqCst);
    assert!(workers.reset_cancel(&cancelled).is_err());
    assert!(cancelled.load(Ordering::SeqCst));
    assert!(workers.spawn("late", || Ok(())).is_err());
    release.send(()).unwrap();
    assert!(workers.join().is_empty());
    assert_eq!(
        workers.snapshot(),
        vec![("producer", WorkerState::Completed)]
    );
}

#[test]
fn adapter_panics_are_reported_and_completed_handles_are_reaped() {
    let workers = Arc::new(ManagedWorkers::new(1));
    workers
        .spawn("panicking", || panic!("controlled worker panic"))
        .unwrap();
    let errors = workers.join();
    assert_eq!(errors, vec!["panicking: worker panicked"]);
    assert!(workers
        .snapshot()
        .iter()
        .all(|(_, state)| !matches!(state, WorkerState::Running)));
}
