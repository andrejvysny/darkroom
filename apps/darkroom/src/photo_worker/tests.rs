//! Unit tests for the photo worker's intake/shutdown primitives and the prepared cache.

use super::*;
use core_model::identity::{
    CacheVersion, CatalogEpoch, DisplayRevision, EditRevision, EditorSession, ImageId,
    ProcessVersion, QualityTier, RenderIdentity, RendererVersion, SelectionGeneration,
    SourceIdentity, SourceRevision, ViewportRevision,
};
use render_wgpu::RenderError;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

fn key(image: i64) -> FrameKey {
    FrameKey {
        identity: RenderIdentity {
            source: SourceIdentity {
                catalog_epoch: CatalogEpoch(1),
                image_id: ImageId(image),
                source_revision: SourceRevision(0),
            },
            editor_session: EditorSession(0),
            selection_generation: SelectionGeneration(0),
            edit_revision: EditRevision(1),
            process_version: ProcessVersion(6),
            renderer_version: RendererVersion(1),
            cache_version: CacheVersion(1),
            viewport_revision: ViewportRevision(0),
            display_revision: DisplayRevision(0),
            device_epoch: DeviceEpoch(1),
        },
        viewport_px: (16, 16),
        scale_milli: 1000,
        quality: QualityTier::Settled,
    }
}

fn request(image: i64) -> PhotoRequest {
    PhotoRequest {
        key: key(image),
        path: PathBuf::from("/nonexistent"),
        content_hash: format!("hash-{image}"),
        params: Arc::new(DevelopParams::default()),
        view: fit_view(16, 16),
    }
}

/// A newer request replaces an unstarted older one: the worker develops the first request it
/// picked up plus the NEWEST of everything queued while it was busy — never the superseded one.
#[test]
fn latest_wins_replaces_an_unstarted_request() {
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in_runner = Arc::clone(&seen);
    let mut worker = PhotoWorker::with_runner(
        Box::new(move |request: PhotoRequest, _cache, _abandon| {
            seen_in_runner.lock().unwrap().push(request.content_hash);
            if seen_in_runner.lock().unwrap().len() == 1 {
                // First request blocks until released, so r2/r3 queue behind it unstarted.
                let _ = release_rx.recv();
            }
            Some(PhotoResult::Failed {
                key: request.key,
                message: "stub".into(),
            })
        }),
        Arc::new(|| {}),
    )
    .unwrap();
    worker.request(request(1));
    // Wait until the runner has actually taken r1 (so r2/r3 are provably unstarted).
    while seen.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    worker.request(request(2));
    worker.request(request(3));
    release_tx.send(()).unwrap();
    let mut finished = Vec::new();
    while finished.len() < 2 {
        for result in worker.drain_results() {
            match result {
                PhotoResult::Failed { key, .. } => finished.push(key.identity.source.image_id.0),
                _ => unreachable!("stub only fails"),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // r1 (already in flight) finishes; the unstarted r2 is replaced by r3.
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["hash-1".to_string(), "hash-3".to_string()]
    );
    assert_eq!(finished, vec![1, 3]);
}

/// Shutdown sets the stop flag, notifies, and joins: after `shutdown` returns the worker
/// thread is dead (no detached thread outliving the join).
#[test]
fn shutdown_joins_the_worker_thread() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let ticks_in_runner = Arc::clone(&ticks);
    let mut worker = PhotoWorker::with_runner(
        Box::new(move |request: PhotoRequest, _cache, _abandon| {
            ticks_in_runner.fetch_add(1, Ordering::SeqCst);
            Some(PhotoResult::Failed {
                key: request.key,
                message: "stub".into(),
            })
        }),
        Arc::new(|| {}),
    )
    .unwrap();
    worker.request(request(1));
    let mut drained = 0;
    while drained == 0 {
        drained = worker.drain_results().len();
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.shutdown().unwrap();
    // Thread joined; submitting more produces nothing and shutdown is idempotent.
    worker.request(request(2));
    assert!(worker.drain_results().is_empty());
    worker.shutdown().unwrap();
    assert_eq!(ticks.load(Ordering::SeqCst), 1);
}

struct Stub(u64);

impl SizedPayload for Stub {
    fn bytes(&self) -> u64 {
        self.0
    }
}

fn k(n: i64) -> (i64, String) {
    (n, "h".into())
}

fn sizes(w: u32, h: u32) -> u64 {
    core_pipeline::backend::estimate_prepare_bytes(w, h)
        + core_pipeline::backend::estimate_mask_bytes(w, h)
}

/// Mimics `prepare_image`: make room first, then insert.
fn admit(cache: &mut PreparedCache<Stub>, n: i64, bytes: u64) {
    cache.make_room(bytes, None);
    assert!(
        cache.resident_bytes() + bytes <= cache.budget_bytes || cache.over_budget,
        "room made before insert"
    );
    cache.put(k(n), Arc::new(Stub(bytes)));
}

/// Rapid selection A,B,C,A: LRU order, get() refreshes, never above the entry cap.
#[test]
fn prepared_cache_lru_and_entry_cap() {
    let mut cache = PreparedCache::<Stub>::with_limits(u64::MAX, 2);
    admit(&mut cache, 1, 1);
    admit(&mut cache, 2, 1);
    assert!(cache.get(&k(1)).is_some(), "refresh A");
    admit(&mut cache, 3, 1);
    assert!(cache.get(&k(2)).is_none(), "B was least recent");
    assert!(cache.get(&k(1)).is_some() && cache.get(&k(3)).is_some());
    admit(&mut cache, 1, 1); // A again (hit path re-put) keeps cap
    assert!(cache.entries.len() <= 2);
    cache.put(k(3), Arc::new(Stub(7)));
    assert_eq!(cache.entries.len(), 2, "re-put replaces in place");
    assert_eq!(cache.resident_bytes(), 8);
}

#[test]
fn make_room_evicts_before_insert_within_budget() {
    let budget = PREPARED_BUDGET_BYTES;
    let mp32 = sizes(6960, 4640);
    let mp60 = sizes(9504, 6336);
    assert!(mp32 < budget && mp60 > budget, "{mp32} {mp60}");
    assert!(2 * mp32 > budget, "two 32MP nominal do not fit");

    let mut cache = PreparedCache::<Stub>::with_limits(budget, 2);
    admit(&mut cache, 1, mp32);
    assert!(!cache.over_budget);
    admit(&mut cache, 2, mp32);
    assert_eq!(cache.entries.len(), 1, "first evicted before second");
    assert!(cache.get(&k(2)).is_some() && !cache.over_budget);
    admit(&mut cache, 3, mp60);
    assert_eq!(
        cache.entries.len(),
        1,
        "60MP incl. masks exceeds budget: others evicted, it opens alone"
    );
    assert!(cache.over_budget);
}

#[test]
fn oversized_single_image_evicts_everything_and_flags_over_budget() {
    let pano = sizes(20000, 8000);
    assert!(pano > PREPARED_BUDGET_BYTES);
    let mut cache = PreparedCache::<Stub>::with_limits(PREPARED_BUDGET_BYTES, 2);
    admit(&mut cache, 1, 1 << 20);
    admit(&mut cache, 2, 1 << 20);
    cache.make_room(pano, None);
    assert!(cache.entries.is_empty(), "all others evicted");
    assert!(cache.over_budget);
    cache.put(k(9), Arc::new(Stub(pano)));
    // The next normal image evicts the pano and clears the flag.
    cache.make_room(1 << 20, None);
    assert!(cache.entries.is_empty() && !cache.over_budget);
}

#[test]
fn make_room_never_evicts_keep() {
    let mut cache = PreparedCache::<Stub>::with_limits(10, 4);
    cache.put(k(1), Arc::new(Stub(6)));
    cache.put(k(2), Arc::new(Stub(3)));
    cache.make_room(8, Some(&k(1)));
    assert!(cache.get(&k(1)).is_some());
    assert!(cache.get(&k(2)).is_none());
    assert!(cache.over_budget, "keep alone + incoming exceeds budget");
}

/// Report sums prepared and pool separately; decode peak is monotonic.
#[test]
fn report_keeps_prepared_and_pool_separate() {
    let mut cache = PreparedCache::<Stub>::with_limits(100, 2);
    admit(&mut cache, 1, 40);
    cache.note_decode(500);
    cache.note_decode(200);
    let pool = render_wgpu::ResourceReport {
        pool_bytes: 7,
        pool_limit_bytes: 70,
    };
    let report = cache.report(pool);
    assert_eq!(report.prepared_gpu_bytes, 40);
    assert_eq!(report.prepared_count, 1);
    assert_eq!((report.pool_bytes, report.pool_limit_bytes), (7, 70));
    assert_eq!(report.last_decode_cpu_bytes, 200);
    assert_eq!(report.peak_decode_cpu_bytes, 500);
    assert!(!report.over_budget);
}

/// `report` copies the in-flight `timings` into `last_timings` (default before any request).
#[test]
fn report_publishes_last_request_timings() {
    let mut cache = PreparedCache::<Stub>::with_limits(100, 2);
    let pool = render_wgpu::ResourceReport {
        pool_bytes: 0,
        pool_limit_bytes: 0,
    };
    assert_eq!(cache.report(pool).last_timings, StageTimings::default());
    let timings = StageTimings {
        read_ms: 120,
        develop_ms: 340,
        prepare_ms: 15,
        render_ms: 8,
        histogram_ms: 4,
        prepared_cache_hit: true,
    };
    cache.timings = timings;
    assert_eq!(cache.report(pool).last_timings, timings);
}

#[test]
fn unsupported_reason_names_the_feature() {
    assert_eq!(unsupported_reason(&DevelopParams::default()), None);
    let denoise = DevelopParams {
        denoise: core_model::Denoise {
            enabled: true,
            amount: 50.0,
        },
        ..Default::default()
    };
    assert_eq!(unsupported_reason(&denoise), Some("AI denoise"));
    // Enabled == off is still off: `is_off` semantics (disabled or 0 amount) render fine.
    let off = DevelopParams {
        denoise: core_model::Denoise {
            enabled: true,
            amount: 0.0,
        },
        ..Default::default()
    };
    assert_eq!(unsupported_reason(&off), None);
}

fn wait_for<T>(worker: &mut PhotoWorker, mut pick: impl FnMut(PhotoResult) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        for result in worker.drain_results() {
            if let Some(found) = pick(result) {
                return found;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for a result");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn runner_panic_reports_worker_died_and_status_stopped() {
    let mut worker = PhotoWorker::with_runner(
        Box::new(|_request, _cache, _abandon| panic!("boom")),
        Arc::new(|| {}),
    )
    .unwrap();
    assert_eq!(worker.status(), WorkerStatus::Running);
    worker.request(request(1));
    let reason = wait_for(&mut worker, |r| match r {
        PhotoResult::WorkerDied(reason) => Some(reason),
        _ => None,
    });
    assert!(reason.contains("boom"), "{reason}");
    assert!(matches!(worker.status(), WorkerStatus::Stopped(r) if r.contains("boom")));
    // The thread ended normally after reporting (panic was caught): join is clean.
    assert_eq!(worker.shutdown(), Ok(()));
}

#[test]
fn channel_disconnection_is_observed_as_stopped() {
    // A runner that returns None ends the loop without a stop request: the receiver sees a
    // disconnected channel and must report the worker as stopped.
    let mut worker =
        PhotoWorker::with_runner(Box::new(|_r, _c, _a| None), Arc::new(|| {})).unwrap();
    worker.request(request(1));
    let deadline = Instant::now() + Duration::from_secs(5);
    while worker.status() == WorkerStatus::Running {
        worker.drain_results();
        assert!(Instant::now() < deadline, "disconnect never observed");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(matches!(worker.status(), WorkerStatus::Stopped(r) if r.contains("exited")));
}

#[test]
fn clean_shutdown_is_not_reported_as_stopped() {
    let mut worker =
        PhotoWorker::with_runner(Box::new(|_r, _c, _a| None), Arc::new(|| {})).unwrap();
    worker.shutdown().unwrap();
    worker.drain_results();
    assert_eq!(worker.status(), WorkerStatus::Running);
}

#[test]
fn spawn_failure_is_an_error_not_a_panic() {
    let result = PhotoWorker::with_spawn(
        Box::new(|_r, _c, _a| None),
        Arc::new(|| {}),
        Box::new(|_body| Err(std::io::Error::other("no threads"))),
    );
    assert!(matches!(result, Err(e) if e.contains("no threads")));
}

/// A request superseded during a stage is abandoned; the newest one is processed.
#[test]
fn superseding_request_abandons_the_running_one() {
    let started = Arc::new(AtomicBool::new(false));
    let started_in_runner = Arc::clone(&started);
    let mut worker = PhotoWorker::with_runner(
        Box::new(move |request: PhotoRequest, _cache, abandon| {
            if request.content_hash == "hash-1" {
                started_in_runner.store(true, Ordering::SeqCst);
                while !abandon() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                return Some(PhotoResult::Abandoned(request.key));
            }
            Some(PhotoResult::Failed {
                key: request.key,
                message: "newest".into(),
            })
        }),
        Arc::new(|| {}),
    )
    .unwrap();
    worker.request(request(1));
    while !started.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    worker.request(request(2));
    let mut seen = Vec::new();
    wait_for(&mut worker, |r| {
        match r {
            PhotoResult::Abandoned(k) => seen.push(("abandoned", k.identity.source.image_id.0)),
            PhotoResult::Failed { key, .. } => {
                seen.push(("failed", key.identity.source.image_id.0))
            }
            _ => {}
        }
        (seen.len() == 2).then_some(())
    });
    assert_eq!(seen, vec![("abandoned", 1), ("failed", 2)]);
}

/// Shutdown while a request sits in pool-exhausted waits returns within ~one retry sleep.
#[test]
fn shutdown_during_pool_wait_is_prompt() {
    let waiting = Arc::new(AtomicBool::new(false));
    let waiting_in_runner = Arc::clone(&waiting);
    let mut worker = PhotoWorker::with_runner(
        Box::new(move |request: PhotoRequest, _cache, abandon| {
            let outcome = render_until_free::<()>(abandon, || {
                waiting_in_runner.store(true, Ordering::SeqCst);
                Err(RenderError::PoolExhausted { size: (16, 16) })
            });
            assert_eq!(outcome, Err(RenderStop::Abandoned));
            Some(PhotoResult::Abandoned(request.key))
        }),
        Arc::new(|| {}),
    )
    .unwrap();
    worker.request(request(1));
    while !waiting.load(Ordering::SeqCst) {
        std::thread::yield_now();
    }
    let started = Instant::now();
    worker.shutdown().unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(200),
        "shutdown took {:?}",
        started.elapsed()
    );
}

/// A thread that dies outside the runner (join failure) surfaces from `shutdown`.
#[test]
fn join_failure_is_reported_by_shutdown() {
    let mut worker = PhotoWorker::with_spawn(
        Box::new(|_r, _c, _a| None),
        Arc::new(|| {}),
        Box::new(|_body| std::thread::Builder::new().spawn(|| panic!("thread blew up"))),
    )
    .unwrap();
    let error = worker.shutdown().unwrap_err();
    assert!(
        error.contains("photo worker panicked: thread blew up"),
        "{error}"
    );
}
