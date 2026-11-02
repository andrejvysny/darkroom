use super::*;

#[test]
fn overflow_keeps_dirty_marks_and_requests_rescan() {
    let q = SidecarQueue::new();
    let ids: Vec<_> = (0..=MAX_PENDING as i64).collect();
    q.mark_dirty_many(&ids);
    q.mark_dirty(0);
    let state = q.lock();
    assert_eq!(state.dirty.len(), MAX_PENDING);
    assert_eq!(state.scan, Some(i64::MIN));
}

#[test]
fn failed_scan_wakes_flush_and_reports_failure() {
    let q = SidecarQueue::new();
    {
        let mut state = q.lock();
        state.scan = Some(i64::MIN);
    }
    assert!(matches!(q.next_batch(), Some(Batch::Scan(_, _))));
    q.fail_batch();
    assert!(!q.flush_blocking(Duration::ZERO));
    assert!(q.next_batch().is_none());
}

#[test]
fn shutdown_drains_dirty_and_rescan() {
    let q = SidecarQueue::new();
    q.mark_dirty(1);
    {
        let mut state = q.lock();
        state.scan = Some(i64::MIN);
    }
    q.shutdown();
    q.mark_dirty(2);
    assert!(matches!(q.next_batch(), Some(Batch::Dirty(ids)) if ids == vec![1]));
    q.finish_batch();
    let Some(Batch::Scan(_, generation)) = q.next_batch() else {
        panic!("expected scan")
    };
    q.finish_scan(generation, &[]);
    assert!(q.next_batch().is_none());
    assert!(q.flush_blocking(Duration::ZERO));
}

/// Pull the pending marks out of the queue the way the worker does, without sitting out the
/// coalesce window.
fn take_dirty(queue: &SidecarQueue) -> Vec<i64> {
    queue.lock().flush_requested = true;
    match queue.next_batch() {
        Some(Batch::Dirty(ids)) => ids,
        other => panic!("expected a dirty batch, got {other:?}"),
    }
}

/// A write that fails on the filesystem must not be counted as done: the id goes back on the
/// dirty set and the queue refuses to report a drain.
#[test]
fn failed_sidecar_write_keeps_the_dirty_mark() {
    let f = core_runtime::sidecar::test_support::Fix::new("queue-failed-write");
    let id = f.insert_image(30, "a.cr3");
    let queue = SidecarQueue::new();
    queue.mark_dirty(id);
    let batch = take_dirty(&queue);
    assert_eq!(batch, vec![id]);
    write_ids(&f.gate, &f.db, &queue, &batch, &|_| {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    })
    .unwrap();
    queue.finish_batch();
    {
        // Scope the guard: `flush_blocking` takes the same mutex, and holding it here would
        // self-deadlock a `std::sync::Mutex`.
        let state = queue.lock();
        assert!(state.deferred.contains_key(&id), "failed write backs off");
        assert!(state.dirty.is_empty());
    }
    assert!(
        !queue.flush_blocking(Duration::ZERO),
        "a failed write must not look like a drained queue"
    );
    assert!(f.sidecar("a.cr3").is_none());
}

/// The other half: a written image and a row that no longer exists both finish their marks.
#[test]
fn written_and_gone_ids_finish_their_marks() {
    let f = core_runtime::sidecar::test_support::Fix::new("queue-written-gone");
    let id = f.insert_image(31, "a.cr3");
    let queue = SidecarQueue::new();
    queue.mark_dirty(id);
    queue.mark_dirty(99_999);
    let batch = take_dirty(&queue);
    assert_eq!(batch.len(), 2);
    write_ids(&f.gate, &f.db, &queue, &batch, &production_write).unwrap();
    queue.finish_batch();
    assert!(queue.lock().dirty.is_empty(), "no id may stay pending");
    assert!(queue.flush_blocking(Duration::ZERO));
    assert!(
        f.sidecar("a.cr3").is_some(),
        "the present image got its sidecar"
    );
}

use std::sync::atomic::{AtomicU64, Ordering};

fn ms(m: u64) -> Duration {
    Duration::from_millis(m)
}

/// Queue on a fake clock: time = `base` + an offset the test advances, so backoff windows are
/// checked without ever sleeping.
fn queued() -> (SidecarQueue, Arc<AtomicU64>, Instant) {
    let base = Instant::now();
    let offset = Arc::new(AtomicU64::new(0));
    let clock = {
        let offset = offset.clone();
        Arc::new(move || base + ms(offset.load(Ordering::SeqCst)))
    };
    (SidecarQueue::with_clock(clock), offset, base)
}

#[test]
fn failed_write_backs_off_and_retries() {
    let (q, _tick, t0) = queued();
    q.defer(1, Some("read-only volume".into()), t0);
    assert!(q.try_next_batch(t0).is_none(), "not due yet");
    assert!(
        matches!(q.try_next_batch(t0 + ms(500)), Some(Batch::Dirty(ids)) if ids == vec![1]),
        "first retry is due at t0 + 500ms"
    );
    assert!(
        q.lock().deferred.contains_key(&1),
        "an in-flight retry keeps its deferred entry"
    );
    q.defer(1, Some("still read-only".into()), t0 + ms(500));
    assert!(
        q.try_next_batch(t0 + ms(500)).is_none(),
        "second failure waits a full 1s"
    );
    assert!(
        matches!(q.try_next_batch(t0 + ms(1500)), Some(Batch::Dirty(ids)) if ids == vec![1]),
        "second retry is due at t0 + 500ms + 1s"
    );
    let st = q.lock();
    let deferred = st.deferred.get(&1).expect("still deferred until success");
    assert_eq!(deferred.attempts, 2);
    assert_eq!(deferred.due, t0 + ms(1500));
}

#[test]
fn new_mark_resets_backoff() {
    let (q, _tick, t0) = queued();
    q.defer(1, Some("read-only volume".into()), t0);
    q.lock().deferred.get_mut(&1).unwrap().attempts = 5;
    q.mark_dirty(1);
    {
        let st = q.lock();
        assert!(
            st.deferred.is_empty(),
            "a fresh mark is new intent, not a retry"
        );
        assert!(st.dirty.contains(&1));
    }
    assert!(
        matches!(q.try_next_batch(t0), Some(Batch::Dirty(ids)) if ids == vec![1]),
        "the reset id goes out immediately, ignoring the 5-attempt backoff"
    );
}

#[test]
fn drain_with_deferred_is_not_success() {
    let (q, _tick, t0) = queued();
    q.defer(1, Some("disk full".into()), t0);
    assert!(
        !q.flush_blocking(Duration::ZERO),
        "deferred entries must not look like a drained queue"
    );
    let report = q.pending_report();
    assert_eq!(report.dirty, 0);
    assert_eq!(report.deferred, 1);
    assert_eq!(report.failing, 1);
    assert!(
        report
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("disk full")),
        "{report:?}"
    );
}

#[test]
fn success_clears_deferred() {
    let f = core_runtime::sidecar::test_support::Fix::new("queue-deferred-success");
    let id = f.insert_image(32, "a.cr3");
    let (queue, tick, t0) = queued();
    queue.mark_dirty(id);
    let batch = take_dirty(&queue);
    write_ids(&f.gate, &f.db, &queue, &batch, &|_| {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    })
    .unwrap();
    queue.finish_batch();
    assert_eq!(queue.pending_report().failing, 1);
    tick.store(500, Ordering::SeqCst);
    let retry = match queue.try_next_batch(t0 + ms(500)) {
        Some(Batch::Dirty(ids)) => ids,
        other => panic!("expected the due retry, got {other:?}"),
    };
    assert_eq!(retry, vec![id]);
    write_ids(&f.gate, &f.db, &queue, &retry, &production_write).unwrap();
    queue.finish_batch();
    let report = queue.pending_report();
    assert_eq!((report.dirty, report.deferred, report.failing), (0, 0, 0));
    assert!(report.last_error.is_none(), "{report:?}");
    assert!(
        queue.flush_blocking(Duration::ZERO),
        "a real success drains the queue"
    );
}

#[test]
fn shutdown_gives_deferred_one_last_attempt() {
    let (q, _tick, t0) = queued();
    q.defer(1, Some("transient".into()), t0);
    q.shutdown();
    assert!(
        matches!(q.next_batch(), Some(Batch::Dirty(ids)) if ids == vec![1]),
        "shutdown hands out every deferred id once regardless of due"
    );
    q.finish_batch(); // the last attempt did not succeed: nothing settled
    assert_eq!(q.pending_report().deferred, 1);
    assert!(
        q.next_batch().is_none(),
        "one last attempt, then the worker exits"
    );
    assert!(
        !q.flush_blocking(Duration::ZERO),
        "a remaining deferred entry is not a drain"
    );
}
