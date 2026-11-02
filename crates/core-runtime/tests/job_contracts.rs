use core_app::jobs::{
    JobClass, JobFailure, JobOwner, JobProgress, JobSpec, JobState, QueuePolicy, SubmitError,
};
use core_model::identity::CatalogEpoch;
use core_runtime::jobs::{Config, JobHandle, Supervisor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

fn spec(class: JobClass, policy: QueuePolicy, bytes: u64) -> JobSpec {
    JobSpec {
        owner: JobOwner {
            catalog_epoch: CatalogEpoch(1),
            editor_session: None,
        },
        class,
        policy,
        estimated_bytes: bytes,
        durable: false,
    }
}

fn make_pool(jobs: usize, bytes: u64, records: usize) -> Supervisor {
    Supervisor::new(
        Config {
            max_jobs: jobs,
            max_bytes: bytes,
            max_terminal_records: records,
            workers: 2,
        },
        Arc::new(|| {}),
    )
    .unwrap()
}

fn receive<T>(rx: &mpsc::Receiver<T>) -> T {
    rx.recv_timeout(Duration::from_secs(5))
        .expect("worker did not signal")
}

/// Occupy both pool workers with blocking priority-0 jobs; releasing the two senders
/// lets them finish.
fn occupy_workers(pool: &Supervisor) -> (Vec<JobHandle<()>>, mpsc::Sender<()>, mpsc::Sender<()>) {
    let (started_tx, started_rx) = mpsc::channel();
    let (release_a, gate_a) = mpsc::channel::<()>();
    let (release_b, gate_b) = mpsc::channel::<()>();
    let first_started = started_tx.clone();
    let blockers = vec![
        pool.submit(
            spec(JobClass::Viewport, QueuePolicy::Ordered, 10),
            move |_| {
                first_started.send(()).unwrap();
                gate_a.recv().unwrap();
                Ok(())
            },
        )
        .unwrap(),
        pool.submit(
            spec(JobClass::UserCommand, QueuePolicy::Ordered, 10),
            move |_| {
                started_tx.send(()).unwrap();
                gate_b.recv().unwrap();
                Ok(())
            },
        )
        .unwrap(),
    ];
    receive(&started_rx);
    receive(&started_rx);
    (blockers, release_a, release_b)
}

#[test]
fn capacity_is_retained_until_result_is_taken_or_handle_dropped() {
    let pool = make_pool(1, 10, 4);
    let s = spec(JobClass::Analysis, QueuePolicy::Ordered, 10);
    let (tx, rx) = mpsc::channel();
    let job = pool
        .submit(s, move |_| {
            tx.send(()).unwrap();
            Ok(7)
        })
        .unwrap();
    receive(&rx);
    // Ensure the worker commits `Succeeded` before shutdown: a cancel request that
    // beats the commit would legitimately retire the uncommitted disposable value.
    while !job.status().is_some_and(|s| s.state.is_terminal()) {
        std::thread::yield_now();
    }
    pool.join();
    assert_eq!(pool.metrics().reserved_bytes, 10);
    assert_eq!(job.wait(), Ok(7));
    assert_eq!(pool.metrics().reserved_bytes, 0);

    let second_pool = make_pool(2, 10, 4);
    let job = second_pool.submit(s, |_| Ok(8)).unwrap();
    assert!(matches!(
        second_pool.submit(s, |_| Ok(9)),
        Err(SubmitError::Capacity)
    ));
    assert_eq!(job.wait(), Ok(8));
    let next = second_pool.submit(s, |_| Ok(9)).unwrap();
    assert_eq!(next.wait(), Ok(9));
    assert_eq!(second_pool.metrics().reserved_bytes, 0);

    let drop_pool = make_pool(1, 10, 4);
    let (started, observed) = mpsc::channel();
    let abandoned = drop_pool
        .submit(s, move |_| {
            started.send(()).unwrap();
            Ok(10)
        })
        .unwrap();
    receive(&observed);
    drop_pool.join();
    assert_eq!(drop_pool.metrics().reserved_bytes, 10);
    drop(abandoned);
    assert_eq!(drop_pool.metrics().reserved_bytes, 0);
}

#[test]
fn ordered_queue_is_fifo_and_reports_backpressure() {
    let pool = make_pool(3, 30, 6);
    let s = spec(JobClass::Analysis, QueuePolicy::Ordered, 10);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = pool
        .submit(s, move |_| {
            started_tx.send(0).unwrap();
            release_rx.recv().unwrap();
            Ok(0)
        })
        .unwrap();
    assert_eq!(receive(&started_rx), 0);
    let (order_tx, order_rx) = mpsc::channel();
    let tx = order_tx.clone();
    let second = pool
        .submit(s, move |_| {
            tx.send(1).unwrap();
            Ok(1)
        })
        .unwrap();
    let third = pool
        .submit(s, move |_| {
            order_tx.send(2).unwrap();
            Ok(2)
        })
        .unwrap();
    assert!(matches!(
        pool.submit(s, |_| Ok(3)),
        Err(SubmitError::Capacity)
    ));
    release_tx.send(()).unwrap();
    assert_eq!(receive(&order_rx), 1);
    assert_eq!(receive(&order_rx), 2);
    assert_eq!(first.wait(), Ok(0));
    assert_eq!(second.wait(), Ok(1));
    assert_eq!(third.wait(), Ok(2));
}

#[test]
fn latest_replaces_queued_job_even_at_capacity() {
    let pool = make_pool(2, 20, 8);
    let blocker = spec(JobClass::Analysis, QueuePolicy::Ordered, 10);
    let latest = spec(JobClass::Analysis, QueuePolicy::Latest { key: 3 }, 10);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit(blocker, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
    receive(&started_rx);
    let old = pool.submit(latest, |_| Ok(1)).unwrap();
    let replacement = pool.submit(latest, |_| Ok(2));
    if replacement.is_err() {
        release_tx.send(()).unwrap();
        panic!("replacement must be admitted at capacity");
    }
    let new = replacement.unwrap();
    assert_eq!(old.wait(), Err(JobFailure::Superseded));
    release_tx.send(()).unwrap();
    assert_eq!(running.wait(), Ok(()));
    assert_eq!(new.wait(), Ok(2));
}

#[test]
fn latest_running_result_is_not_published_after_replacement() {
    let pool = make_pool(3, 30, 8);
    let s = spec(JobClass::Analysis, QueuePolicy::Latest { key: 4 }, 10);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let old = pool
        .submit(s, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(1)
        })
        .unwrap();
    receive(&started_rx);
    let new = pool.submit(s, |_| Ok(2)).unwrap();
    assert_eq!(old.status().unwrap().state, JobState::CancelRequested);
    release_tx.send(()).unwrap();
    assert_eq!(old.wait(), Err(JobFailure::Cancelled));
    assert_eq!(new.wait(), Ok(2));
}

#[test]
fn dedup_returns_existing_job_id() {
    let pool = make_pool(2, 20, 4);
    let s = spec(JobClass::Analysis, QueuePolicy::Deduplicated { key: 8 }, 10);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(s, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
        .unwrap();
    receive(&started_rx);
    assert!(
        matches!(pool.submit(s, |_| Ok(())), Err(SubmitError::Duplicate(id)) if id == job.id())
    );
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Ok(()));
}

#[test]
fn foreground_lane_runs_while_background_busy_and_prefetch_pending() {
    let pool = make_pool(3, 30, 6);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let background = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
        )
        .unwrap();
    receive(&started_rx);
    let (prefetch_tx, prefetch_rx) = mpsc::channel();
    let prefetch = pool
        .submit(
            spec(JobClass::Prefetch, QueuePolicy::Ordered, 10),
            move |_| {
                prefetch_tx.send(()).unwrap();
                Ok(())
            },
        )
        .unwrap();
    let (foreground_tx, foreground_rx) = mpsc::channel();
    let foreground = pool
        .submit(
            spec(JobClass::Viewport, QueuePolicy::Ordered, 10),
            move |_| {
                foreground_tx.send(()).unwrap();
                Ok(())
            },
        )
        .unwrap();
    receive(&foreground_rx);
    assert!(prefetch_rx.try_recv().is_err());
    release_tx.send(()).unwrap();
    receive(&prefetch_rx);
    assert_eq!(background.wait(), Ok(()));
    assert_eq!(foreground.wait(), Ok(()));
    assert_eq!(prefetch.wait(), Ok(()));
}

#[test]
fn cancellation_waits_for_cooperative_checkpoint() {
    let pool = make_pool(1, 10, 4);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |ctx| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                ctx.checkpoint()?;
                Ok(())
            },
        )
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    assert_eq!(job.status().unwrap().state, JobState::CancelRequested);
    assert!(job.try_take().is_none());
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Err(JobFailure::Cancelled));
}

#[test]
fn durable_success_after_cancel_remains_success() {
    let pool = make_pool(1, 10, 4);
    let mut s = spec(JobClass::Export, QueuePolicy::Ordered, 10);
    s.durable = true;
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(s, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(42)
        })
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    assert_eq!(job.status().unwrap().state, JobState::CancelRequested);
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Ok(42));
    assert_eq!(job.status().unwrap().state, JobState::Succeeded);
}

#[test]
fn shutdown_cancels_queued_disposable_and_drains_durable() {
    let pool = make_pool(3, 30, 6);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let running = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            },
        )
        .unwrap();
    receive(&started_rx);
    let disposable = pool
        .submit(spec(JobClass::Analysis, QueuePolicy::Ordered, 10), |_| {
            Ok(1)
        })
        .unwrap();
    let mut durable_spec = spec(JobClass::Export, QueuePolicy::Ordered, 10);
    durable_spec.durable = true;
    let durable = pool.submit(durable_spec, |_| Ok(2)).unwrap();
    pool.stop();
    assert_eq!(disposable.wait(), Err(JobFailure::Cancelled));
    release_tx.send(()).unwrap();
    pool.join();
    assert_eq!(running.wait(), Err(JobFailure::Cancelled));
    assert_eq!(durable.wait(), Ok(2));
    assert!(matches!(
        pool.submit(durable_spec, |_| Ok(())),
        Err(SubmitError::Stopped)
    ));
}

#[test]
fn panic_does_not_kill_worker() {
    let pool = make_pool(1, 10, 4);
    let s = spec(JobClass::Analysis, QueuePolicy::Ordered, 10);
    let panic = pool
        .submit(s, |_| -> Result<(), JobFailure> { panic!("contract test") })
        .unwrap();
    assert_eq!(panic.wait(), Err(JobFailure::Panic));
    let next = pool.submit(s, |_| Ok(3)).unwrap();
    assert_eq!(next.wait(), Ok(3));
}

#[test]
fn latest_progress_survives_disconnected_listener_and_terminal_result() {
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    drop(wake_rx);
    let pool = Supervisor::new(
        Config {
            max_jobs: 1,
            max_bytes: 10,
            max_terminal_records: 4,
            workers: 2,
        },
        Arc::new(move || {
            let _ = wake_tx.send(());
        }),
    )
    .unwrap();
    let (progress_tx, progress_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |ctx| {
                ctx.progress(1, 3);
                ctx.progress(2, 3);
                progress_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(5)
            },
        )
        .unwrap();
    receive(&progress_rx);
    assert_eq!(
        job.status().unwrap().progress,
        Some(JobProgress { done: 2, total: 3 })
    );
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Ok(5));
    assert_eq!(job.status().unwrap().state, JobState::Succeeded);
}

#[test]
fn acknowledged_terminal_records_are_bounded_under_burst() {
    let pool = make_pool(1, 10, 3);
    let s = spec(JobClass::Analysis, QueuePolicy::Ordered, 10);
    for value in 0..20 {
        let job = pool.submit(s, move |_| Ok(value)).unwrap();
        assert_eq!(job.wait(), Ok(value));
        let id = job.id();
        drop(job);
        assert!(pool.forget(id));
        assert!(pool.metrics().retained <= 3);
    }
    assert!(pool.snapshot().len() <= 3);
}

#[test]
fn ordered_lane_serializes_effects_and_terminal_handles_remain_queryable() {
    let pool = make_pool(4, 40, 4);
    let (started, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let first = pool
        .submit(
            spec(JobClass::UserCommand, QueuePolicy::Ordered, 10),
            move |_| {
                started.send(()).unwrap();
                proceed.recv().unwrap();
                Ok(1)
            },
        )
        .unwrap();
    receive(&observed);
    let second = pool
        .submit(
            spec(JobClass::UserCommand, QueuePolicy::Ordered, 10),
            |_| Ok(2),
        )
        .unwrap();
    assert_eq!(second.status().unwrap().state, JobState::Queued);
    release.send(()).unwrap();
    assert_eq!(first.wait(), Ok(1));
    assert_eq!(second.wait(), Ok(2));
    for _ in 0..8 {
        let next = pool
            .submit(spec(JobClass::UserCommand, QueuePolicy::Ordered, 1), |_| {
                Ok(())
            })
            .unwrap();
        next.wait().unwrap();
    }
    assert_eq!(first.status().unwrap().state, JobState::Succeeded);
    assert!(pool.snapshot().len() <= 4);
    pool.join();
}

#[test]
fn completed_context_cannot_publish_progress_or_continue_work() {
    let pool = make_pool(2, 20, 4);
    let (context, receiver) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::UserCommand, QueuePolicy::Ordered, 1),
            move |ctx| {
                context.send(ctx).unwrap();
                Ok(())
            },
        )
        .unwrap();
    let ctx = receive(&receiver);
    job.wait().unwrap();
    assert!(ctx.checkpoint().is_err());
    ctx.progress(9, 10);
    assert_eq!(job.status().unwrap().progress, None);
    pool.join();
}

#[test]
fn rejected_latest_intent_still_discards_obsolete_running_result() {
    let pool = make_pool(1, 10, 4);
    let s = spec(JobClass::Viewport, QueuePolicy::Latest { key: 42 }, 10);
    let (started, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let old = pool
        .submit(s, move |_| {
            started.send(()).unwrap();
            proceed.recv().unwrap();
            Ok(1)
        })
        .unwrap();
    receive(&observed);
    let rejected = pool.submit(s, |_| Ok(2));
    let state = old.status().unwrap().state;
    release.send(()).unwrap();
    assert!(matches!(rejected, Err(SubmitError::Capacity)));
    assert_eq!(state, JobState::CancelRequested);
    assert_eq!(old.wait(), Err(JobFailure::Cancelled));
    assert_eq!(pool.metrics().reserved_bytes, 0);
}

#[test]
fn oversized_latest_intent_supersedes_queued_work_without_escaping_budget() {
    let pool = make_pool(2, 20, 4);
    let (started, observed) = mpsc::channel();
    let (release, proceed) = mpsc::channel();
    let blocker = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| {
                started.send(()).unwrap();
                proceed.recv().unwrap();
                Ok(())
            },
        )
        .unwrap();
    receive(&observed);
    let latest = spec(JobClass::Analysis, QueuePolicy::Latest { key: 43 }, 10);
    let old = pool.submit(latest, |_| Ok(1)).unwrap();
    let mut oversized = latest;
    oversized.estimated_bytes = 21;
    let rejected = pool.submit(oversized, |_| Ok(2));
    release.send(()).unwrap();
    assert!(matches!(rejected, Err(SubmitError::Capacity)));
    assert_eq!(old.wait(), Err(JobFailure::Superseded));
    assert_eq!(blocker.wait(), Ok(()));
    assert_eq!(pool.metrics().reserved_bytes, 0);
}

#[test]
fn fallible_domain_error_is_failed_and_delivers_typed_error() {
    let pool = make_pool(1, 10, 4);
    let job = pool
        .submit_fallible(spec(JobClass::Analysis, QueuePolicy::Ordered, 10), |_| {
            Ok(Err::<(), String>("boom".to_string()))
        })
        .unwrap();
    assert_eq!(job.wait(), Ok(Err("boom".to_string())));
    assert_eq!(job.status().unwrap().state, JobState::Failed);
}

#[test]
fn worker_survives_domain_failure_and_next_job_succeeds() {
    let pool = make_pool(1, 10, 4);
    let failing = pool
        .submit_fallible(spec(JobClass::Analysis, QueuePolicy::Ordered, 10), |_| {
            Ok(Err::<(), String>("boom".to_string()))
        })
        .unwrap();
    assert_eq!(failing.wait(), Ok(Err("boom".to_string())));
    let next = pool
        .submit(spec(JobClass::Analysis, QueuePolicy::Ordered, 10), |_| {
            Ok(7)
        })
        .unwrap();
    assert_eq!(next.wait(), Ok(7));
    assert_eq!(next.status().unwrap().state, JobState::Succeeded);
    assert_eq!(pool.worker_failures(), 0);
}

#[test]
fn fallible_success_is_succeeded() {
    let pool = make_pool(1, 10, 4);
    let job = pool
        .submit_fallible(spec(JobClass::Analysis, QueuePolicy::Ordered, 10), |_| {
            Ok(Ok::<i32, &str>(1))
        })
        .unwrap();
    assert_eq!(job.wait(), Ok(Ok(1)));
    assert_eq!(job.status().unwrap().state, JobState::Succeeded);
}

#[test]
fn domain_failure_wins_over_cancel_request() {
    let pool = make_pool(1, 10, 4);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit_fallible(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Err::<(), &str>("late failure"))
            },
        )
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    assert_eq!(job.status().unwrap().state, JobState::CancelRequested);
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Ok(Err("late failure")));
    assert_eq!(job.status().unwrap().state, JobState::Failed);
}

#[test]
fn cancel_and_supersede_stay_distinct() {
    let pool = make_pool(1, 10, 4);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit_fallible(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Ok::<(), &str>(()))
            },
        )
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Err(JobFailure::Cancelled));
    assert_eq!(job.status().unwrap().state, JobState::Cancelled);
}

#[test]
fn panic_under_cancel_request_is_failed() {
    let pool = make_pool(2, 20, 8);
    let (started_tx, started_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| -> Result<i32, JobFailure> {
                started_tx.send(()).unwrap();
                panic!("boom");
            },
        )
        .unwrap();
    receive(&started_rx);
    job.cancel();
    assert_eq!(job.wait(), Err(JobFailure::Panic));
    assert_eq!(job.status().unwrap().state, JobState::Failed);
}

#[test]
fn superseded_under_cancel_request_stays_superseded() {
    let pool = make_pool(2, 20, 8);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| -> Result<i32, JobFailure> {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err(JobFailure::Superseded)
            },
        )
        .unwrap();
    receive(&started_rx);
    job.cancel();
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Err(JobFailure::Superseded));
    assert_eq!(job.status().unwrap().state, JobState::Superseded);
}

#[test]
fn domain_failure_terminal_snapshot_is_retained_after_wait() {
    let pool = make_pool(1, 10, 4);
    let job = pool
        .submit_fallible(spec(JobClass::Analysis, QueuePolicy::Ordered, 10), |_| {
            Ok(Err::<(), &str>("boom"))
        })
        .unwrap();
    let id = job.id();
    assert_eq!(job.wait(), Ok(Err("boom")));
    assert!(pool
        .snapshot()
        .iter()
        .any(|record| record.id == id && record.state == JobState::Failed));
}

#[test]
fn cancel_after_commit_is_interrupted_and_delivers_payload() {
    let pool = make_pool(2, 20, 8);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Import, QueuePolicy::Ordered, 10),
            move |ctx| {
                ctx.mark_committed();
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(3)
            },
        )
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Ok(3));
    assert_eq!(job.status().unwrap().state, JobState::Interrupted);
}

#[test]
fn cancel_before_commit_still_cancelled() {
    let pool = make_pool(2, 20, 8);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Import, QueuePolicy::Ordered, 10),
            move |_| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(3)
            },
        )
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    release_tx.send(()).unwrap();
    assert_eq!(job.wait(), Err(JobFailure::Cancelled));
    assert_eq!(job.status().unwrap().state, JobState::Cancelled);
}

#[test]
fn task_returning_cancelled_after_commit_is_interrupted() {
    let pool = make_pool(2, 20, 8);
    let (started_tx, started_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Import, QueuePolicy::Ordered, 10),
            move |ctx| -> Result<i32, JobFailure> {
                ctx.mark_committed();
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                observed_tx.send(ctx.is_cancel_requested()).unwrap();
                Err(JobFailure::Cancelled)
            },
        )
        .unwrap();
    receive(&started_rx);
    assert!(job.cancel());
    release_tx.send(()).unwrap();
    assert!(receive(&observed_rx));
    assert_eq!(job.wait(), Err(JobFailure::Cancelled));
    assert_eq!(job.status().unwrap().state, JobState::Interrupted);
}

/// A task whose durable writes committed is never reported `Superseded`: the late
/// supersede ends `Interrupted` and the result still reaches the handle.
#[test]
fn superseded_after_commit_is_interrupted() {
    let pool = make_pool(2, 20, 8);
    let job = pool
        .submit(
            spec(JobClass::Import, QueuePolicy::Ordered, 10),
            move |ctx| -> Result<i32, JobFailure> {
                ctx.mark_committed();
                Err(JobFailure::Superseded)
            },
        )
        .unwrap();
    assert_eq!(job.wait(), Err(JobFailure::Superseded));
    assert_eq!(job.status().unwrap().state, JobState::Interrupted);
}

#[test]
fn is_committed_reflects_mark() {
    let pool = make_pool(2, 20, 8);
    let (observed_tx, observed_rx) = mpsc::channel();
    let job = pool
        .submit(
            spec(JobClass::Import, QueuePolicy::Ordered, 10),
            move |ctx| -> Result<(), JobFailure> {
                let before = ctx.is_committed();
                ctx.mark_committed();
                let after = ctx.is_committed();
                observed_tx.send((before, after)).unwrap();
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(job.wait(), Ok(()));
    assert_eq!(receive(&observed_rx), (false, true));
}

#[test]
fn stop_cancels_queued_nondurable_but_not_durable() {
    let pool = make_pool(4, 40, 8);
    let (blockers, release_a, release_b) = occupy_workers(&pool);
    let ran = Arc::new(AtomicBool::new(false));
    let ran_in_task = ran.clone();
    let disposable = pool
        .submit(
            spec(JobClass::Analysis, QueuePolicy::Ordered, 10),
            move |_| {
                ran_in_task.store(true, Ordering::SeqCst);
                Ok(1)
            },
        )
        .unwrap();
    let mut durable_spec = spec(JobClass::Export, QueuePolicy::Ordered, 10);
    durable_spec.durable = true;
    let durable = pool.submit(durable_spec, |_| Ok(2)).unwrap();
    pool.stop();
    pool.stop();
    assert_eq!(disposable.wait(), Err(JobFailure::Cancelled));
    assert_eq!(disposable.status().unwrap().state, JobState::Cancelled);
    let pending: Vec<_> = pool
        .pending_after_stop()
        .iter()
        .map(|record| record.id)
        .collect();
    assert!(pending.contains(&durable.id()));
    for blocker in &blockers {
        assert!(pending.contains(&blocker.id()));
    }
    assert!(!pending.contains(&disposable.id()));
    release_a.send(()).unwrap();
    release_b.send(()).unwrap();
    assert_eq!(durable.wait(), Ok(2));
    assert_eq!(durable.status().unwrap().state, JobState::Succeeded);
    for blocker in blockers {
        // Non-durable running jobs were only cancel-*requested* by stop; they kept
        // running (they were never retired early) but land in `Cancelled` on return.
        assert_eq!(blocker.wait(), Err(JobFailure::Cancelled));
        assert_eq!(blocker.status().unwrap().state, JobState::Cancelled);
    }
    assert!(!ran.load(Ordering::SeqCst));
    assert!(pool.pending_after_stop().is_empty());
}

/// The trash/resolve commands rely on this: `cull_delete_rejected`, `dedup_resolve` and the flag
/// mutations they must not race with are all `UserCommand` + `Ordered` jobs with the same owner,
/// so the worker's candidate filter never lets one overlap another — a photo cannot be
/// un-rejected between a delete's plan and finalize.
#[test]
fn ordered_user_commands_with_same_owner_never_overlap() {
    let pool = make_pool(4, 100, 8);
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let first = pool
        .submit(
            spec(JobClass::UserCommand, QueuePolicy::Ordered, 10),
            move |_| {
                release_rx.recv().unwrap(); // blocks until released
                Ok(())
            },
        )
        .unwrap();
    while !first.status().is_some_and(|s| s.state == JobState::Running) {
        std::thread::yield_now();
    }

    let (second_started_tx, second_started_rx) = mpsc::channel::<()>();
    let second = pool
        .submit(
            spec(JobClass::UserCommand, QueuePolicy::Ordered, 10),
            move |_| {
                second_started_tx.send(()).unwrap();
                Ok(())
            },
        )
        .unwrap();

    // The first job owns the lane: the second must sit queued, not started.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        matches!(second_started_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "second Ordered UserCommand started while the first was still running"
    );

    release_tx.send(()).unwrap();
    receive(&second_started_rx); // released -> the lane opens for the second job
    assert_eq!(first.wait(), Ok(()));
    assert_eq!(second.wait(), Ok(()));
    pool.join();
}
