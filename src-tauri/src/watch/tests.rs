use super::PublicationFence;
use std::sync::{mpsc, Arc, Mutex};

#[test]
fn supported_paths_match_watcher_rules() {
    assert!(super::supported_path(std::path::Path::new("image.CR3")));
    assert!(!super::supported_path(std::path::Path::new("._image.CR3")));
    assert!(!super::supported_path(std::path::Path::new("image.txt")));
}

#[test]
fn retry_wake_coalesces_without_blocking() {
    let fence = PublicationFence::default();
    let (sender, receiver) = mpsc::sync_channel(1);
    *fence.wake.lock().unwrap() = Some(sender.clone());
    fence.request_retry();
    sender.try_send(()).unwrap();
    assert!(matches!(
        sender.try_send(()),
        Err(mpsc::TrySendError::Full(()))
    ));
    assert!(fence.take_retry_if_idle());
    receiver.try_recv().unwrap();
    assert!(receiver.try_recv().is_err());
}

#[test]
fn publication_completed_during_decode_invalidates_snapshot_and_retries() {
    let fence = PublicationFence::default();
    let db = Mutex::new(());
    let generation = fence.snapshot(&db.lock().unwrap()).unwrap();
    // The watcher owns decoded bytes from before a whole import/export guard lifetime.
    fence.begin(&db);
    fence.end(&db);
    assert!(!fence.is_active());
    assert!(!fence.is_current(&db.lock().unwrap(), generation));
    // No guard remains to drain this late request, so the watcher must schedule it itself.
    fence.request_retry();
    assert!(fence.take_retry_if_idle());
    assert!(!fence.take_retry_if_idle());
}

#[test]
fn overlapping_publications_defer_retry_until_the_last_finishes() {
    let fence = PublicationFence::default();
    let db = Mutex::new(());
    let generation = fence.snapshot(&db.lock().unwrap()).unwrap();
    fence.begin(&db);
    fence.begin(&db);
    fence.request_retry();
    fence.end(&db);
    assert!(fence.snapshot(&db.lock().unwrap()).is_none());
    assert!(!fence.take_retry_if_idle());
    fence.end(&db);
    assert!(!fence.is_current(&db.lock().unwrap(), generation));
    assert!(fence.take_retry_if_idle());
}

#[test]
fn publication_start_waits_for_watcher_check_and_commit_lock() {
    let fence = Arc::new(PublicationFence::default());
    let db = Arc::new(Mutex::new(()));
    let insertion = db.lock().unwrap();
    let generation = fence.snapshot(&insertion).unwrap();
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let writer = {
        let fence = Arc::clone(&fence);
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            fence.begin(&db);
            started_tx.send(()).unwrap();
        })
    };
    attempt_rx.recv().unwrap();
    // The real begin helper shares the insertion mutex: it cannot cross this check/commit.
    assert!(started_rx.try_recv().is_err());
    assert!(fence.is_current(&insertion, generation));
    drop(insertion);
    started_rx.recv().unwrap();
    writer.join().unwrap();
    assert!(!fence.is_current(&db.lock().unwrap(), generation));
    fence.end(&db);
}
