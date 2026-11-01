//! Unit tests for the photo worker's intake/shutdown primitives and the prepared cache.

use super::*;
use core_model::identity::{
    CacheVersion, CatalogEpoch, DisplayRevision, EditRevision, EditorSession, ImageId,
    ProcessVersion, QualityTier, RenderIdentity, RendererVersion, SelectionGeneration,
    SourceIdentity, SourceRevision, ViewportRevision,
};
use std::sync::atomic::{AtomicUsize, Ordering};

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
    let worker = PhotoWorker::with_runner(
        Box::new(move |request: PhotoRequest, _cache| {
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
    );
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
                PhotoResult::Frame(_) => unreachable!("stub never frames"),
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
        Box::new(move |request: PhotoRequest, _cache| {
            ticks_in_runner.fetch_add(1, Ordering::SeqCst);
            Some(PhotoResult::Failed {
                key: request.key,
                message: "stub".into(),
            })
        }),
        Arc::new(|| {}),
    );
    worker.request(request(1));
    let mut drained = 0;
    while drained == 0 {
        drained = worker.drain_results().len();
        std::thread::sleep(Duration::from_millis(5));
    }
    worker.shutdown();
    // Thread joined; submitting more produces nothing and shutdown is idempotent.
    worker.request(request(2));
    assert!(worker.drain_results().is_empty());
    worker.shutdown();
    assert_eq!(ticks.load(Ordering::SeqCst), 1);
}

/// The prepared cache holds at most two entries and evicts the oldest key.
#[test]
fn prepared_cache_bounds_to_two_entries() {
    let mut cache = PreparedCache::<u32>::new();
    let prepared = |w| Arc::new(w);
    cache.put((1, "a".into()), prepared(1));
    cache.put((2, "b".into()), prepared(2));
    assert!(cache.get(&(1, "a".into())).is_some());
    cache.put((3, "c".into()), prepared(3));
    assert!(
        cache.get(&(1, "a".into())).is_none(),
        "oldest evicted at the cap of 2"
    );
    assert!(cache.get(&(2, "b".into())).is_some());
    assert!(cache.get(&(3, "c".into())).is_some());
    // Re-putting an existing key replaces in place without evicting.
    cache.put((2, "b".into()), prepared(4));
    assert!(cache.get(&(2, "b".into())).is_some());
    assert!(cache.get(&(3, "c".into())).is_some());
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
