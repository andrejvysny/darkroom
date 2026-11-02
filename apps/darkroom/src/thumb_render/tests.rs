use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, Instant};

fn key(id: i64) -> ThumbKey {
    ThumbKey {
        image_id: id,
        hash: format!("{id:064x}"),
        edited_at: Some(1),
    }
}

fn wait_for(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn queue_dedups_caps_and_serves_newest_first() {
    let order = Arc::new(Mutex::new(Vec::<i64>::new()));
    let open = Arc::new(AtomicBool::new(false));
    let body: Arc<RenderBody> = {
        let (order, open) = (Arc::clone(&order), Arc::clone(&open));
        Arc::new(move |job| {
            order.lock().unwrap().push(job.key.image_id);
            while !open.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Rendered::Written)
        })
    };
    let renderer = EditedThumbRenderer::start(body, Box::new(|| 0), Arc::new(|| {})).unwrap();
    // Job 0 runs (and blocks); everything after it queues behind it.
    renderer.ensure(key(0), PathBuf::from("x"));
    wait_for(|| order.lock().unwrap().len() == 1);
    renderer.ensure(key(0), PathBuf::from("x")); // running: not queued again
    let total = PENDING_CAP as i64 + 10;
    for id in 1..=total {
        renderer.ensure(key(id), PathBuf::from("x"));
        renderer.ensure(key(id), PathBuf::from("x")); // duplicate of a queued key
    }
    open.store(true, Ordering::SeqCst);
    wait_for(|| order.lock().unwrap().len() == 1 + PENDING_CAP);
    std::thread::sleep(Duration::from_millis(30));
    let seen = order.lock().unwrap().clone();
    assert_eq!(seen.len(), 1 + PENDING_CAP, "no duplicate, capped backlog");
    assert_eq!(seen[1], total, "newest first");
    assert!(!seen.contains(&1), "oldest dropped beyond the cap");
    let mut unique = seen.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), seen.len());
}

#[test]
fn results_are_reported_and_dropped_jobs_are_silent() {
    let calls = Arc::new(AtomicUsize::new(0));
    let body: Arc<RenderBody> = {
        let calls = Arc::clone(&calls);
        Arc::new(move |job| {
            calls.fetch_add(1, Ordering::SeqCst);
            match job.key.image_id {
                1 => Ok(Rendered::Written),
                2 => Ok(Rendered::Dropped),
                _ => Err("nope".into()),
            }
        })
    };
    let mut renderer = EditedThumbRenderer::start(body, Box::new(|| 0), Arc::new(|| {})).unwrap();
    for id in 1..=3 {
        renderer.ensure(key(id), PathBuf::from("x"));
    }
    wait_for(|| calls.load(Ordering::SeqCst) == 3);
    let mut events = Vec::new();
    wait_for(|| {
        events.extend(renderer.drain());
        events.len() == 2
    });
    assert!(events
        .iter()
        .any(|e| matches!(e, RenderEvent::Done(k) if k.image_id == 1)));
    assert!(events
        .iter()
        .any(|e| matches!(e, RenderEvent::Failed(k, _) if k.image_id == 3)));
    // An unedited key is never queued.
    let unedited = ThumbKey {
        edited_at: None,
        ..key(4)
    };
    renderer.ensure(unedited, PathBuf::from("x"));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(renderer.shutdown().is_ok());
    assert!(renderer.shutdown().is_ok());
}
