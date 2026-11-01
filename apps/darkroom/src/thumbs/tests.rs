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
