//! Disk preview loader for the Develop/loupe stage: while a photo's live render is still owed,
//! the stage shows the best geometry-exact picture the cache already holds. The small thumbnail
//! is a resident texture ([`crate::thumbs::ThumbStore`]); this one thread reads the LARGE tier
//! (the display-sharp preview written at import/render), decodes it off the UI thread and hands
//! RGBA back for the host to upload. One request slot, latest wins: opening photos quickly never
//! queues decodes of photos nobody is looking at any more.

use crate::file_stamp::{remove_if_unchanged, stamp, Stamp};
use core_library::ThumbCache;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

/// What to look for. `edited_at: Some(v)` wants the exact edited tier of that edit version;
/// `None` the unedited tiers (also used for the Before variant, which shows the unedited photo).
#[derive(Clone, Debug)]
pub struct PreviewQuery {
    /// Echoed in the result so the host can drop answers for a photo it has moved on from.
    pub token: u64,
    pub hash: String,
    pub edited_at: Option<i64>,
    pub edge: u32,
}

pub struct PreviewResult {
    pub token: u64,
    pub size: [u32; 2],
    pub rgba: Vec<u8>,
}

/// The files that can answer `query`, best first. Only tiers of the SAME framing: an edited
/// photo is never answered by its unedited preview (that would show an uncropped picture).
pub fn candidates(cache: &ThumbCache, pv: i64, query: &PreviewQuery) -> Vec<PathBuf> {
    match query.edited_at {
        Some(version) => vec![cache.edited_preview_path_for(&query.hash, pv, version, query.edge)],
        None => vec![
            cache.preview_path_for(&query.hash, pv, query.edge),
            cache.embedded_preview_path_for(&query.hash, query.edge),
        ],
    }
}

#[derive(Default)]
struct Slot {
    request: Option<PreviewQuery>,
    stop: bool,
}

type Shared = Arc<(Mutex<Slot>, Condvar)>;

fn lock(slot: &Mutex<Slot>) -> MutexGuard<'_, Slot> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct StagePreview {
    shared: Shared,
    results: Receiver<PreviewResult>,
    thread: Option<JoinHandle<()>>,
}

impl StagePreview {
    /// `wake` redraws the window when a result is ready.
    pub fn start(
        cache: ThumbCache,
        pv: i64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let shared: Shared = Arc::default();
        let (tx, results) = mpsc::channel();
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("stage-preview".to_owned())
            .spawn(move || worker_loop(&worker, &cache, pv, &tx, &*wake))
            .map_err(|error| format!("could not start the stage preview loader: {error}"))?;
        Ok(Self {
            shared,
            results,
            thread: Some(thread),
        })
    }

    /// Replace whatever was queued (latest wins).
    pub fn request(&self, query: PreviewQuery) {
        lock(&self.shared.0).request = Some(query);
        self.shared.1.notify_one();
    }

    /// Finished decodes since the last call, oldest first.
    pub fn drain(&self) -> Vec<PreviewResult> {
        self.results.try_iter().collect()
    }
}

impl Drop for StagePreview {
    fn drop(&mut self) {
        lock(&self.shared.0).stop = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn next_request(shared: &Shared) -> Option<PreviewQuery> {
    let mut guard = lock(&shared.0);
    loop {
        if guard.stop {
            return None;
        }
        if let Some(request) = guard.request.take() {
            return Some(request);
        }
        guard = shared.1.wait(guard).unwrap_or_else(PoisonError::into_inner);
    }
}

/// Stop requested, or a newer request waits: the decode in hand is for a photo no longer wanted.
fn superseded(shared: &Shared) -> bool {
    let slot = lock(&shared.0);
    slot.stop || slot.request.is_some()
}

enum Read {
    Missing,
    /// The file could not be read (permissions, I/O, too many open files): skip it, never delete.
    Unreadable(String),
    /// The bytes do not decode; `stamp` identifies the exact file version that was read.
    Corrupt(String, Option<Stamp>),
    Ok(PreviewResult),
}

fn read(path: &std::path::Path, token: u64) -> Read {
    let before = stamp(path);
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Read::Missing,
        Err(error) => return Read::Unreadable(error.to_string()),
    };
    match image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg) {
        Ok(image) => {
            let image = image.to_rgba8();
            Read::Ok(PreviewResult {
                token,
                size: [image.width(), image.height()],
                rgba: image.into_raw(),
            })
        }
        Err(error) => Read::Corrupt(error.to_string(), before),
    }
}

fn worker_loop(
    shared: &Shared,
    cache: &ThumbCache,
    pv: i64,
    tx: &Sender<PreviewResult>,
    wake: &(dyn Fn() + Send + Sync),
) {
    while let Some(query) = next_request(shared) {
        for path in candidates(cache, pv, &query) {
            if superseded(shared) {
                break;
            }
            match read(&path, query.token) {
                Read::Missing => {}
                // A preview that cannot be decoded would be retried on every selection: remove
                // it (the next render rewrites it) and fall back to the next tier.
                Read::Corrupt(error, read_stamp) => {
                    eprintln!("stage preview {} corrupt: {error}", path.display());
                    remove_if_unchanged(&path, read_stamp);
                }
                Read::Unreadable(error) => {
                    eprintln!("stage preview {} unreadable: {error}", path.display());
                }
                Read::Ok(result) => {
                    if tx.send(result).is_ok() {
                        wake();
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "darkroom-stage-preview-{}-{}",
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

    fn query(token: u64, hash: &str, edited_at: Option<i64>) -> PreviewQuery {
        PreviewQuery {
            token,
            hash: hash.to_owned(),
            edited_at,
            edge: 64,
        }
    }

    fn started(dir: &TempDir) -> (StagePreview, ThumbCache) {
        let cache = ThumbCache::new(&dir.0).unwrap();
        let preview = StagePreview::start(cache.clone(), 6, Arc::new(|| {})).unwrap();
        (preview, cache)
    }

    fn wait(preview: &StagePreview) -> Option<PreviewResult> {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if let Some(result) = preview.drain().pop() {
                return Some(result);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn edited_exact_never_falls_back_to_unedited_tiers() {
        let dir = TempDir::new();
        let cache = ThumbCache::new(&dir.0).unwrap();
        let edited = candidates(&cache, 6, &query(1, "ab", Some(9)));
        assert_eq!(edited, vec![cache.edited_preview_path_for("ab", 6, 9, 64)]);
        let plain = candidates(&cache, 6, &query(1, "ab", None));
        assert_eq!(
            plain,
            vec![
                cache.preview_path_for("ab", 6, 64),
                cache.embedded_preview_path_for("ab", 64)
            ]
        );
    }

    #[test]
    fn decodes_the_best_tier_and_echoes_the_token() {
        let dir = TempDir::new();
        let (preview, cache) = started(&dir);
        std::fs::write(cache.embedded_preview_path_for("ab", 64), jpeg(20, 10)).unwrap();
        preview.request(query(7, "ab", None));
        let result = wait(&preview).expect("decoded");
        assert_eq!((result.token, result.size), (7, [20, 10]));
        assert_eq!(result.rgba.len(), 20 * 10 * 4);
    }

    #[test]
    fn a_corrupt_file_is_deleted_and_the_next_tier_answers() {
        let dir = TempDir::new();
        let (preview, cache) = started(&dir);
        let bad = cache.preview_path_for("cd", 6, 64);
        std::fs::write(&bad, b"not a jpeg").unwrap();
        std::fs::write(cache.embedded_preview_path_for("cd", 64), jpeg(8, 8)).unwrap();
        preview.request(query(3, "cd", None));
        assert_eq!(wait(&preview).expect("fallback").size, [8, 8]);
        assert!(!bad.exists(), "the undecodable tier is removed");
    }

    #[test]
    fn a_corrupt_only_tier_yields_nothing_and_is_removed() {
        let dir = TempDir::new();
        let (preview, cache) = started(&dir);
        let bad = cache.edited_preview_path_for("ef", 6, 2, 64);
        std::fs::write(&bad, b"garbage").unwrap();
        preview.request(query(4, "ef", Some(2)));
        let end = Instant::now() + Duration::from_secs(5);
        while bad.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!bad.exists());
        assert!(preview.drain().is_empty());
    }

    #[test]
    fn the_request_slot_is_latest_wins() {
        // No worker: only the slot semantics matter.
        let shared: Shared = Arc::default();
        for token in 1..=3 {
            lock(&shared.0).request = Some(query(token, "ab", None));
        }
        assert_eq!(next_request(&shared).map(|q| q.token), Some(3));
        lock(&shared.0).stop = true;
        assert!(next_request(&shared).is_none());
    }

    #[test]
    fn a_queued_newer_request_supersedes_the_decode_in_hand() {
        let shared: Shared = Arc::default();
        assert!(!superseded(&shared));
        lock(&shared.0).request = Some(query(2, "ab", None));
        assert!(superseded(&shared));
    }
}
