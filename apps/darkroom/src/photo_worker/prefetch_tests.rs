//! Neighbour prefetch: cancellation, the one-image handoff, upload priority, admission, shutdown.

use super::*;
use crate::photo_worker::inbox::{next_job, Slot};
use crate::photo_worker::prepared::{PreparedCache, SizedPayload};
use crate::photo_worker::{Job, PhotoRequest, PhotoWorker};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn warm(image: i64) -> WarmRequest {
    WarmRequest {
        image_id: image,
        path: std::path::PathBuf::from("/nonexistent"),
        content_hash: format!("hash-{image}"),
    }
}

fn key(image: i64) -> SourceKey {
    (image, format!("hash-{image}"))
}

fn image() -> LinearImage {
    LinearImage {
        width: 1,
        height: 1,
        data: vec![0.0; 3],
    }
}

fn inbox() -> Arc<SlotPair> {
    Arc::new((
        Mutex::new(Slot {
            request: None,
            warm: None,
            stop: false,
            pool_returns: 0,
        }),
        Condvar::new(),
    ))
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A prefetch with its thread running `decode`; returns the join handle.
fn spawn(
    decode: impl Fn(&WarmRequest, &Cancel<'_>) -> Option<LinearImage> + Send + 'static,
) -> (Arc<Prefetch>, std::thread::JoinHandle<()>) {
    let prefetch = Prefetch::new(inbox());
    let thread = {
        let prefetch = Arc::clone(&prefetch);
        std::thread::spawn(move || run(&prefetch, &decode))
    };
    (prefetch, thread)
}

fn finish(prefetch: &Prefetch, thread: std::thread::JoinHandle<()>) {
    prefetch.stop();
    thread.join().unwrap();
}

/// A new list cancels the in-flight decode at its next stage check, and the old list's remaining
/// requests are never decoded.
#[test]
fn new_list_cancels_the_in_flight_decode_at_the_next_stage() {
    let (stage_tx, stage_rx) = mpsc::channel::<i64>();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let go_rx = Mutex::new(go_rx);
    let cancelled = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&cancelled);
    let (prefetch, thread) = spawn(move |request, cancel| {
        stage_tx.send(request.image_id).unwrap();
        if request.image_id == 1 {
            // "read" done; wait while the test submits a newer list, then hit the stage check.
            go_rx.lock().unwrap().recv().unwrap();
            let gone = cancel.is_cancelled();
            log.lock().unwrap().push((request.image_id, gone));
            return (!gone).then(image);
        }
        Some(image())
    });
    prefetch.submit(vec![warm(1), warm(2)]);
    assert_eq!(stage_rx.recv().unwrap(), 1);
    prefetch.submit(vec![warm(3)]);
    go_tx.send(()).unwrap();
    assert_eq!(stage_rx.recv().unwrap(), 3, "2 of the old list was skipped");
    assert_eq!(*cancelled.lock().unwrap(), vec![(1, true)]);
    until("3 parked", || prefetch.parked() == Some(key(3)));
    finish(&prefetch, thread);
}

/// At most one image is parked, and the thread does not decode the next one until the worker is
/// done with the taken image (it waits BEFORE decoding).
#[test]
fn handoff_holds_one_image_and_blocks_the_decoder() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let (prefetch, thread) = spawn(move |_, _| {
        counted.fetch_add(1, Ordering::SeqCst);
        Some(image())
    });
    prefetch.submit(vec![warm(1), warm(2), warm(3)]);
    until("first parked", || prefetch.parked() == Some(key(1)));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "full slot: no second decode"
    );

    let (taken, _) = prefetch.take().expect("parked image");
    assert_eq!(taken, key(1));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "slot stays occupied while the worker holds the image"
    );
    prefetch.release();
    until("second parked", || prefetch.parked() == Some(key(2)));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    finish(&prefetch, thread);
}

/// Sources the worker already has are not decoded again.
#[test]
fn known_sources_are_skipped() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let (prefetch, thread) = spawn(move |request, _| {
        seen.lock().unwrap().push(request.image_id);
        Some(image())
    });
    prefetch.set_known([key(1)].into_iter().collect());
    prefetch.submit(vec![warm(1), warm(2)]);
    until("2 parked", || prefetch.parked() == Some(key(2)));
    assert_eq!(*calls.lock().unwrap(), vec![2]);
    finish(&prefetch, thread);
}

/// A render for a parked source takes the decode (no second decode); another source does not.
#[test]
fn promotion_takes_only_the_matching_parked_image() {
    let (prefetch, thread) = spawn(|_, _| Some(image()));
    prefetch.submit(vec![warm(1)]);
    until("1 parked", || prefetch.parked() == Some(key(1)));
    assert!(prefetch.take_matching(&key(2)).is_none());
    assert!(prefetch.take_matching(&key(1)).is_some());
    assert!(prefetch.parked().is_none());
    finish(&prefetch, thread);
}

/// Upload is the lowest-priority job: a queued render, then a queued warm-up, run first.
#[test]
fn upload_runs_only_when_no_render_or_warm_is_queued() {
    let slot = inbox();
    let prefetch = Prefetch::new(Arc::clone(&slot));
    prefetch.state().slot = Some((key(7), image()));
    {
        let mut inbox = slot.0.lock().unwrap();
        inbox.request = Some(PhotoRequest {
            key: crate::photo_worker::tests::key(1),
            path: "/nonexistent".into(),
            content_hash: "hash-1".into(),
            params: Arc::new(Default::default()),
            view: crate::photo_worker::fit_view(16, 16),
        });
        inbox.warm = Some(warm(2));
    }
    assert!(matches!(
        next_job(&slot, Some(&prefetch)),
        Some(Job::Render(_))
    ));
    assert!(matches!(
        next_job(&slot, Some(&prefetch)),
        Some(Job::Warm(_))
    ));
    assert!(matches!(
        next_job(&slot, Some(&prefetch)),
        Some(Job::Upload(d)) if d.0 == key(7)
    ));
}

struct Stub(u64);

impl SizedPayload for Stub {
    fn bytes(&self) -> u64 {
        self.0
    }
}

/// Speculation evicts only other speculative entries, never the pinned/used ones and never
/// beyond the budget; a refusal evicts nothing.
#[test]
fn admission_never_evicts_pinned_and_never_exceeds_budget() {
    let mut cache = PreparedCache::<Stub>::with_limits(10, 4);
    cache.put(key(1), Arc::new(Stub(6))); // the photo on screen
    assert!(!cache.pin(&key(1)));
    assert!(cache.admit_speculative(3));
    cache.put_speculative(key(2), Arc::new(Stub(3)));
    // 6 pinned + 3 speculative: a 4-byte neighbour replaces the speculative one.
    assert!(cache.admit_speculative(4));
    assert!(!cache.contains(&key(2)));
    cache.put_speculative(key(3), Arc::new(Stub(4)));
    // 5 bytes cannot fit next to the pinned 6 even with every speculative entry gone.
    assert!(!cache.admit_speculative(5));
    assert!(cache.contains(&key(3)), "refusal evicts nothing");
    assert!(cache.contains(&key(1)));
    assert!(cache.resident_bytes() <= cache.budget_bytes);
    assert!(!cache.over_budget);
}

/// Render/warm room-making keeps the pinned photo when another source comes in.
#[test]
fn make_room_for_another_source_keeps_the_pinned_one() {
    let mut cache = PreparedCache::<Stub>::with_limits(10, 4);
    cache.put(key(1), Arc::new(Stub(6)));
    cache.pin(&key(1));
    cache.make_room(8, None);
    assert!(cache.contains(&key(1)), "pinned survives");
    assert!(cache.over_budget);
}

/// A speculative entry later used by a render reports a prefetch hit and is no longer evictable
/// by further speculation.
#[test]
fn using_a_speculative_entry_is_a_prefetch_hit() {
    let mut cache = PreparedCache::<Stub>::with_limits(10, 4);
    cache.put_speculative(key(2), Arc::new(Stub(3)));
    cache.put(key(1), Arc::new(Stub(4)));
    assert!(cache.pin(&key(2)), "first use of a speculative entry");
    assert!(
        cache.pin(&key(2)),
        "a following render of it still reports the hit"
    );
    assert!(!cache.pin(&key(1)));
    assert!(
        !cache.admit_speculative(5),
        "used entries are not speculative"
    );
    assert!(cache.known_sources().contains(&key(1)));
}

/// `stop` frees a decoder blocked on a full handoff (nobody takes the parked image).
#[test]
fn stop_wakes_a_decoder_blocked_on_a_full_handoff() {
    let (prefetch, thread) = spawn(|_, _| Some(image()));
    prefetch.submit(vec![warm(1), warm(2)]);
    until("slot filled", || prefetch.parked().is_some());
    std::thread::sleep(Duration::from_millis(30));
    let started = Instant::now();
    finish(&prefetch, thread);
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// The worker's shutdown joins both threads promptly.
#[test]
fn shutdown_joins_both_threads_while_the_handoff_is_full() {
    let mut worker =
        PhotoWorker::with_runner(Box::new(|_job, _cache, _ctl| None), Arc::new(|| {})).unwrap();
    let decoded = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&decoded);
    worker
        .start_prefetch(Box::new(move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(image())
        }))
        .unwrap();
    worker.prefetch(vec![warm(1), warm(2)]);
    until("decoding started", || decoded.load(Ordering::SeqCst) > 0);
    let started = Instant::now();
    worker.shutdown().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(worker.prefetch_thread.is_none() && worker.thread.is_none());
}
