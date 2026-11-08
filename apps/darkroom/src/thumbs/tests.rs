use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static SEQ: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "darkroom-thumbs-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([90, 120, 200]));
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
    out.into_inner()
}

fn key(id: i64, edited_at: Option<i64>) -> ThumbKey {
    ThumbKey {
        image_id: id,
        hash: format!("{id:064x}"),
        edited_at,
    }
}

fn job(id: i64) -> ThumbJob {
    ThumbJob {
        key: key(id, None),
        path: PathBuf::from(format!("/nowhere/{id}.cr3")),
    }
}

#[test]
fn cache_tiers_win_in_legacy_order() {
    let dir = TempDir::new();
    let cache = ThumbCache::new(dir.0.join("thumbs")).unwrap();
    let job = ThumbJob {
        key: key(1, Some(7)),
        path: dir.0.join("missing.cr3"),
    };
    let hash = &job.key.hash;
    cache.write(hash, THUMB_SIZE, &jpeg(4, 4)).unwrap();
    assert_eq!(load(&cache, 6, &job).unwrap().size, [4, 4]);
    cache.write_canonical(hash, 6, &jpeg(6, 4)).unwrap();
    assert_eq!(load(&cache, 6, &job).unwrap().size, [6, 4]);
    cache.write_edited(hash, 6, 7, &jpeg(8, 4)).unwrap();
    assert_eq!(load(&cache, 6, &job).unwrap().size, [8, 4]);
    // Another process version or edit revision is not this thumbnail's edited tier.
    let other = ThumbJob {
        key: key(1, Some(8)),
        ..job.clone()
    };
    assert_eq!(load(&cache, 6, &other).unwrap().size, [6, 4]);
    assert_eq!(load(&cache, 5, &other).unwrap().size, [4, 4]);
}

#[test]
fn corrupt_tier_is_deleted_and_the_next_tier_serves() {
    let dir = TempDir::new();
    let cache = ThumbCache::new(dir.0.join("thumbs")).unwrap();
    let job = ThumbJob {
        key: key(1, Some(7)),
        path: dir.0.join("missing.cr3"),
    };
    let hash = &job.key.hash;
    cache.write(hash, THUMB_SIZE, &jpeg(4, 4)).unwrap();
    cache.write_edited(hash, 6, 7, b"not a jpeg").unwrap();
    let edited = cache.edited_path_for(hash, 6, 7);
    let loaded = load(&cache, 6, &job).unwrap();
    assert_eq!(loaded.size, [4, 4]);
    assert!(loaded.provisional);
    assert!(!edited.exists(), "corrupt tier must be removed");
    assert!(cache.has(hash, THUMB_SIZE), "good tier must survive");
}

#[test]
fn unreadable_tier_is_skipped_never_deleted() {
    let dir = TempDir::new();
    let cache = ThumbCache::new(dir.0.join("thumbs")).unwrap();
    let job = ThumbJob {
        key: key(1, Some(7)),
        path: dir.0.join("missing.cr3"),
    };
    let hash = &job.key.hash;
    cache.write(hash, THUMB_SIZE, &jpeg(4, 4)).unwrap();
    // A directory where the file belongs: read fails with an I/O error that is not NotFound.
    let edited = cache.edited_path_for(hash, 6, 7);
    std::fs::create_dir(&edited).unwrap();
    assert_eq!(load(&cache, 6, &job).unwrap().size, [4, 4]);
    assert!(edited.is_dir(), "unreadable entry must be left alone");
}

#[test]
fn changed_file_is_not_deleted_as_corrupt() {
    let dir = TempDir::new();
    let path = dir.0.join("t.jpg");
    std::fs::write(&path, b"bad").unwrap();
    let before = crate::file_stamp::stamp(&path);
    std::fs::write(&path, jpeg(4, 4)).unwrap();
    crate::file_stamp::remove_if_unchanged(&path, before);
    assert!(path.exists());
    crate::file_stamp::remove_if_unchanged(&path, None);
    assert!(path.exists(), "no stamp, no delete");
}

#[test]
fn hash_mismatch_fails_without_writing() {
    let dir = TempDir::new();
    let cache = ThumbCache::new(dir.0.join("thumbs")).unwrap();
    let source = dir.0.join("photo.jpg");
    std::fs::write(&source, jpeg(16, 16)).unwrap();
    let job = ThumbJob {
        key: key(2, None),
        path: source,
    };
    let error = load(&cache, 6, &job)
        .err()
        .expect("hash mismatch must fail");
    assert!(error.contains("no longer matches"), "{error}");
    assert!(!cache.has(&job.key.hash, THUMB_SIZE));
}

#[test]
fn matching_source_is_generated_and_cached() {
    let dir = TempDir::new();
    let cache = ThumbCache::new(dir.0.join("thumbs")).unwrap();
    let source = dir.0.join("photo.jpg");
    let bytes = jpeg(16, 16);
    std::fs::write(&source, &bytes).unwrap();
    let hash = core_raw::hex(&core_raw::content_hash(&bytes));
    let job = ThumbJob {
        key: ThumbKey {
            image_id: 3,
            hash: hash.clone(),
            edited_at: None,
        },
        path: source,
    };
    assert!(load(&cache, 6, &job).unwrap().size[0] > 0);
    assert!(cache.has(&hash, THUMB_SIZE));
}

#[test]
fn queue_replacement_keeps_visual_order_and_drops_stale() {
    let mut queue = Queue::default();
    queue.replace(vec![job(1), job(2), job(3)]);
    assert_eq!(queue.pop().map(|j| j.key.image_id), Some(1));
    // Scrolled: 1 is running (held), 2 stays, 3 is gone, 9 and 8 are new, in this order.
    queue.replace(vec![job(9), job(1), job(2), job(8)]);
    let order: Vec<i64> = std::iter::from_fn(|| queue.pop())
        .map(|j| j.key.image_id)
        .collect();
    assert_eq!(order, vec![9, 2, 8]);
}

#[test]
fn eviction_is_by_bytes_and_spares_this_frames_requests() {
    let mut residents = Residents::<()>::default();
    for (id, frame) in [(1, 1), (2, 2), (3, 3), (4, 4)] {
        residents.insert(key(id, None), (), [10, 10], frame); // 400 bytes each
    }
    assert_eq!(residents.bytes(), 1600);
    // Frame 5 asks for 4 only: evict oldest-requested first until <= 900 bytes.
    residents.touch(&key(4, None), 5);
    residents.evict(5, 900);
    assert!(!residents.contains(&key(1, None)) && !residents.contains(&key(2, None)));
    assert!(residents.contains(&key(3, None)) && residents.contains(&key(4, None)));
    assert_eq!(residents.bytes(), 800);
    // Over budget but everything is on screen: nothing is evicted.
    residents.touch(&key(3, None), 6);
    residents.touch(&key(4, None), 6);
    residents.evict(6, 100);
    assert_eq!(residents.bytes(), 800);
}

#[test]
fn pool_delivers_results_and_shuts_down() {
    let loader: Arc<Loader> = Arc::new(|job| {
        if job.key.image_id == 13 {
            panic!("boom");
        }
        Ok(Decoded {
            size: [1, 1],
            rgba: vec![0, 0, 0, 255],
            provisional: false,
        })
    });
    let mut pool = Pool::start(2, loader, Arc::new(|| {})).unwrap();
    lock(&pool.shared.0).replace(vec![job(1), job(13)]);
    pool.shared.1.notify_all();
    let mut seen = Vec::new();
    for _ in 0..2 {
        let done = pool.results.recv_timeout(Duration::from_secs(10)).unwrap();
        seen.push((done.key.image_id, done.outcome.is_ok()));
    }
    seen.sort_unstable();
    assert_eq!(seen, vec![(1, true), (13, false)]);
    assert!(pool.shutdown().is_ok());
    assert!(pool.shutdown().is_ok());
}

#[test]
fn previous_edit_version_stands_in_until_the_new_one_lands() {
    let mut residents = Residents::default();
    residents.insert(key(1, Some(10)), "old", [4, 4], 1);
    residents.insert(key(2, Some(10)), "other image", [4, 4], 1);
    let (value, _) = residents
        .touch_previous(&key(1, Some(20)), 2)
        .expect("same image, older edit");
    assert_eq!(*value, "old");
    assert!(residents.touch_previous(&key(3, None), 2).is_none());
}

mod provisional {
    use super::*;
    use crate::thumb_render::{EditedThumbRenderer, RenderBody, Rendered};
    use core_model::catalog::ImageRow;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// Edited versions whose tier "exists"; the loader answers from it like the cache would.
    #[derive(Default)]
    struct Tier {
        rendered: Mutex<HashSet<i64>>,
        calls: AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
        hold: std::sync::atomic::AtomicBool,
    }

    fn pixel(edge: u32, provisional: bool) -> Decoded {
        Decoded {
            size: [edge, edge],
            rgba: vec![0; (4 * edge * edge) as usize],
            provisional,
        }
    }

    fn rig(tier: &Arc<Tier>) -> ThumbStore {
        let loader: Arc<Loader> = {
            let tier = Arc::clone(tier);
            Arc::new(move |job| {
                let version = job.key.edited_at;
                let settled = version.is_some_and(|v| tier.rendered.lock().unwrap().contains(&v));
                Ok(match version {
                    Some(v) if settled => pixel(4 * v as u32, false),
                    Some(_) => pixel(2, true),
                    None => pixel(3, false),
                })
            })
        };
        let body: Arc<RenderBody> = {
            let tier = Arc::clone(tier);
            Arc::new(move |job| {
                tier.calls.fetch_add(1, Ordering::SeqCst);
                while tier.hold.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if tier.fail.load(Ordering::SeqCst) {
                    return Err("render failed".into());
                }
                tier.rendered
                    .lock()
                    .unwrap()
                    .insert(job.key.edited_at.unwrap());
                Ok(Rendered::Written)
            })
        };
        let renderer = EditedThumbRenderer::start(body, Box::new(|| 0), Arc::new(|| {})).unwrap();
        ThumbStore::with_loader(egui::Context::default(), loader, Arc::new(|| {}), 1 << 30)
            .unwrap()
            .with_renderer(renderer)
    }

    fn row(edited_at: Option<i64>) -> ImageRow {
        ImageRow {
            id: 1,
            content_hash: format!("{:064x}", 1),
            path: "/nowhere/1.cr3".into(),
            edited_at,
            ..Default::default()
        }
    }

    fn frame(store: &mut ThumbStore, row: &ImageRow) -> ThumbState {
        store.begin_frame(1);
        let state = store.thumb(row);
        store.end_frame();
        state
    }

    fn edge(state: &ThumbState) -> Option<u32> {
        match state {
            ThumbState::Ready { size, .. } => Some(size[0]),
            _ => None,
        }
    }

    fn settle(store: &mut ThumbStore, row: &ImageRow, want: u32) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while edge(&frame(store, row)) != Some(want) {
            assert!(Instant::now() < deadline, "never reached edge {want}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn provisional_edited_key_is_rendered_once_then_reads_the_edited_tier() {
        let tier = Arc::new(Tier::default());
        let mut store = rig(&tier);
        let row = row(Some(5));
        settle(&mut store, &row, 2); // provisional texture shows first
        settle(&mut store, &row, 20); // Done dropped it; the reload read the edited tier
        assert_eq!(tier.calls.load(Ordering::SeqCst), 1);
        for _ in 0..5 {
            assert_eq!(edge(&frame(&mut store, &row)), Some(20));
        }
        assert_eq!(tier.calls.load(Ordering::SeqCst), 1);
        assert!(store.shutdown().is_ok());
    }

    #[test]
    fn a_provisional_resident_is_not_an_exact_picture_of_its_edit() {
        let tier = Arc::new(Tier::default());
        let mut store = rig(&tier);
        let row = row(Some(5));
        let key = ThumbKey {
            image_id: row.id,
            hash: row.content_hash.clone(),
            edited_at: Some(5),
        };
        settle(&mut store, &row, 2); // provisional stand-in is resident
        assert!(
            store.exact_resident(&key).is_none(),
            "provisional is rejected"
        );
        settle(&mut store, &row, 20);
        let (_, size) = store.exact_resident(&key).expect("settled edit is exact");
        assert_eq!(size, [20, 20]);
        assert!(store
            .exact_resident(&ThumbKey {
                edited_at: Some(6),
                ..key
            })
            .is_none());
    }

    #[test]
    fn unedited_keys_never_reach_the_renderer() {
        let tier = Arc::new(Tier::default());
        let mut store = rig(&tier);
        settle(&mut store, &row(None), 3);
        assert_eq!(tier.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn previous_settled_texture_stays_up_while_the_new_edit_is_provisional() {
        let tier = Arc::new(Tier::default());
        let mut store = rig(&tier);
        settle(&mut store, &row(Some(1)), 2);
        settle(&mut store, &row(Some(1)), 4);
        tier.hold.store(true, Ordering::SeqCst);
        let next = row(Some(2));
        let mut shown = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        // Frames until the provisional result for edit 2 has landed (it is resident).
        while !store.residents.is_provisional(&ThumbKey {
            image_id: 1,
            hash: next.content_hash.clone(),
            edited_at: Some(2),
        }) {
            assert!(Instant::now() < deadline);
            shown.push(edge(&frame(&mut store, &next)));
            std::thread::sleep(Duration::from_millis(2));
        }
        shown.push(edge(&frame(&mut store, &next)));
        assert!(shown.iter().all(|edge| *edge == Some(4)), "{shown:?}");
        tier.hold.store(false, Ordering::SeqCst);
        settle(&mut store, &next, 8);
    }

    #[test]
    fn renderer_failure_keeps_the_provisional_texture_and_is_not_retried() {
        let tier = Arc::new(Tier::default());
        tier.fail.store(true, Ordering::SeqCst);
        let mut store = rig(&tier);
        let row = row(Some(5));
        settle(&mut store, &row, 2);
        let deadline = Instant::now() + Duration::from_secs(10);
        while tier.calls.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        for _ in 0..50 {
            assert_eq!(edge(&frame(&mut store, &row)), Some(2));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(tier.calls.load(Ordering::SeqCst), 1);
    }
}
