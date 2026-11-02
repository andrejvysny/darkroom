use super::*;

#[test]
fn overflow_requests_paged_rescan_and_keeps_bound() {
    let q = ThumbQueue::new();
    q.enqueue_bulk(0..(MAX_PENDING as i64 + 1));
    let state = q.lock();
    assert_eq!(state.queued.len(), MAX_PENDING);
    assert_eq!(state.scan, Some(i64::MIN));
}

#[test]
fn scan_refills_after_pending_jobs_and_shutdown_stops() {
    let q = ThumbQueue::new();
    q.request_scan();
    let Some(NextJob::Refill(cursor, generation)) = q.next_job() else {
        panic!("expected refill")
    };
    assert_eq!(cursor, i64::MIN);
    q.refill(generation, vec![1, 2]);
    assert!(matches!(q.next_job(), Some(NextJob::Render(1))));
    assert!(matches!(q.next_job(), Some(NextJob::Render(2))));
    assert!(q.lock().scan.is_none());
    q.shutdown();
    assert!(q.next_job().is_none());
}
