//! Bounded asynchronous thumbnail service behind `ui_egui::ThumbSource`.
//!
//! The UI thread never touches the disk: [`ThumbStore::thumb`] answers from resident textures and
//! records what was asked. After the frame, [`ThumbStore::end_frame`] REPLACES the pending queue
//! with the visible-but-missing keys in the order the UI asked for them (latest wins; off-screen
//! requests of earlier frames are dropped). Two worker threads read the cache tiers the legacy
//! `thumb://` protocol used (edited -> canonical -> plain 512), generate the plain thumbnail from
//! the hash-verified source when no tier exists, decode to RGBA and hand the result back; the next
//! frame uploads it as an egui texture under a byte budget with least-recently-requested
//! eviction. Failures are remembered (bounded) so they are not retried every frame.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use core_library::{ThumbCache, THUMB_SIZE};
use ui_egui::{ThumbSource, ThumbState};

#[cfg(test)]
#[path = "thumbs/tests.rs"]
mod tests;

/// Uploaded thumbnail textures stay under this many bytes (4 bytes per pixel) unless the current
/// frame alone needs more.
pub const THUMB_TEXTURE_BUDGET: u64 = 192 * 1024 * 1024;
/// Remembered failures; the oldest is forgotten first.
const FAILED_CAP: usize = 4096;
/// Most distinct rows one frame may ask for (a grid shows far fewer).
const ASKED_CAP: usize = 1024;
const WORKERS: usize = 2;
/// Import-dialog source previews kept (textures and remembered failures); least recently asked
/// for goes first.
const PREVIEW_CAP: usize = 64;
/// Source previews queued or decoding at once; more are asked for again on a later frame.
const PREVIEW_INFLIGHT: usize = 8;
const PREVIEW_EDGE: u32 = 512;

/// What a thumbnail is of: a new `edited_at` or content hash is a new key.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ThumbKey {
    pub image_id: i64,
    pub hash: String,
    pub edited_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct ThumbJob {
    pub key: ThumbKey,
    pub path: PathBuf,
}

pub struct Decoded {
    pub size: [u32; 2],
    pub rgba: Vec<u8>,
}

struct Done {
    key: ThumbKey,
    outcome: Result<Decoded, String>,
}

/// Produces pixels for a job; the production one reads/generates through the cache.
pub type Loader = dyn Fn(&ThumbJob) -> Result<Decoded, String> + Send + Sync + 'static;

#[derive(Default)]
struct Queue {
    pending: VecDeque<ThumbJob>,
    /// Keys a worker is running or whose result the UI has not drained yet.
    held: HashSet<ThumbKey>,
    stop: bool,
}

impl Queue {
    /// Latest wins: `jobs` (visual order) become the whole pending queue, minus held keys.
    fn replace(&mut self, jobs: Vec<ThumbJob>) {
        self.pending = jobs
            .into_iter()
            .filter(|job| !self.held.contains(&job.key))
            .collect();
    }

    fn pop(&mut self) -> Option<ThumbJob> {
        let job = self.pending.pop_front()?;
        self.held.insert(job.key.clone());
        Some(job)
    }
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Worker threads plus the queue they share.
struct Pool {
    shared: Shared,
    results: Receiver<Done>,
    threads: Vec<JoinHandle<()>>,
}

impl Pool {
    fn start(
        workers: usize,
        loader: Arc<Loader>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let (tx, results) = mpsc::channel();
        let mut pool = Self {
            shared: Arc::default(),
            results,
            threads: Vec::new(),
        };
        for index in 0..workers {
            let (shared, tx) = (Arc::clone(&pool.shared), tx.clone());
            let (loader, wake) = (Arc::clone(&loader), Arc::clone(&wake));
            let spawned = std::thread::Builder::new()
                .name(format!("thumb-worker-{index}"))
                .spawn(move || worker_loop(&shared, &tx, &*loader, &*wake));
            match spawned {
                Ok(handle) => pool.threads.push(handle),
                // `pool` drops here and joins the workers already started.
                Err(error) => return Err(format!("could not start thumbnail worker: {error}")),
            }
        }
        Ok(pool)
    }

    fn shutdown(&mut self) -> Result<(), String> {
        let (queue, cvar) = &*self.shared;
        lock(queue).stop = true;
        cvar.notify_all();
        let died = self
            .threads
            .drain(..)
            .filter_map(|thread| thread.join().err())
            .count();
        if died == 0 {
            Ok(())
        } else {
            Err(format!("{died} thumbnail worker thread(s) panicked"))
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("{error}");
        }
    }
}

fn worker_loop(
    shared: &(Mutex<Queue>, Condvar),
    tx: &Sender<Done>,
    loader: &Loader,
    wake: &(dyn Fn() + Send + Sync),
) {
    let (queue, cvar) = shared;
    loop {
        let job = {
            let mut guard = lock(queue);
            loop {
                if guard.stop {
                    return;
                }
                if let Some(job) = guard.pop() {
                    break job;
                }
                guard = cvar.wait(guard).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loader(&job)))
            .unwrap_or_else(|payload| {
                Err(format!(
                    "thumbnail worker panicked: {}",
                    crate::photo_worker::panic_text(&*payload)
                ))
            });
        let done = Done {
            key: job.key,
            outcome,
        };
        if tx.send(done).is_err() {
            return; // host gone
        }
        wake();
    }
}

/// Texture residency under a byte budget. Generic over the payload so tests need no egui.
pub struct Residents<T> {
    entries: HashMap<ThumbKey, Entry<T>>,
    bytes: u64,
}

struct Entry<T> {
    value: T,
    size: [u32; 2],
    bytes: u64,
    last_used: u64,
}

impl<T> Default for Residents<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
        }
    }
}

impl<T> Residents<T> {
    pub fn touch(&mut self, key: &ThumbKey, frame: u64) -> Option<(&T, [u32; 2])> {
        let entry = self.entries.get_mut(key)?;
        entry.last_used = frame;
        Some((&entry.value, entry.size))
    }

    pub fn insert(&mut self, key: ThumbKey, value: T, size: [u32; 2], frame: u64) {
        let bytes = 4 * u64::from(size[0]) * u64::from(size[1]);
        let entry = Entry {
            value,
            size,
            bytes,
            last_used: frame,
        };
        if let Some(old) = self.entries.insert(key, entry) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
    }

    /// Drop least-recently-requested entries NOT requested in `frame` until under `budget`.
    pub fn evict(&mut self, frame: u64, budget: u64) {
        while self.bytes > budget {
            let victim = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.last_used < frame)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            let Some(entry) = victim.and_then(|key| self.entries.remove(&key)) else {
                break; // everything left is on screen
            };
            self.bytes -= entry.bytes;
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Any resident texture of the same image bytes under another edit version: shown while the
    /// current version loads, so an edit never flashes the cell back to a placeholder.
    pub fn touch_previous(&mut self, key: &ThumbKey, frame: u64) -> Option<(&T, [u32; 2])> {
        let previous = self
            .entries
            .iter()
            .filter(|(other, _)| other.image_id == key.image_id && other.hash == key.hash)
            .max_by_key(|(other, _)| other.edited_at)
            .map(|(other, _)| other.clone())?;
        self.touch(&previous, frame)
    }
}

/// UI-thread side: implements [`ThumbSource`].
pub struct ThumbStore {
    ctx: egui::Context,
    pool: Pool,
    residents: Residents<egui::TextureHandle>,
    failed: HashSet<ThumbKey>,
    failed_order: VecDeque<ThumbKey>,
    frame: u64,
    catalog_epoch: Option<u64>,
    asked: Vec<ThumbJob>,
    asked_keys: HashSet<ThumbKey>,
    budget: u64,
    previews: Previews,
}

impl ThumbStore {
    /// `cache_root` is `<catalog dir>/thumbs`. `wake` redraws the window when a result lands.
    pub fn new(
        ctx: egui::Context,
        cache_root: PathBuf,
        process_version: i64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let cache = ThumbCache::new(cache_root)
            .map_err(|error| format!("could not open the thumbnail cache: {error}"))?;
        let loader: Arc<Loader> = Arc::new(move |job| load(&cache, process_version, job));
        Self::with_loader(ctx, loader, wake, THUMB_TEXTURE_BUDGET)
    }

    pub fn with_loader(
        ctx: egui::Context,
        loader: Arc<Loader>,
        wake: Arc<dyn Fn() + Send + Sync>,
        budget: u64,
    ) -> Result<Self, String> {
        let wake_for_previews = Arc::clone(&wake);
        Ok(Self {
            ctx,
            pool: Pool::start(WORKERS, loader, wake)?,
            residents: Residents::default(),
            failed: HashSet::new(),
            failed_order: VecDeque::new(),
            frame: 0,
            catalog_epoch: None,
            asked: Vec::new(),
            asked_keys: HashSet::new(),
            budget,
            previews: Previews::new(wake_for_previews),
        })
    }

    /// Start of a UI frame: upload finished thumbnails; a new catalog epoch forgets failures.
    pub fn begin_frame(&mut self, catalog_epoch: u64) {
        self.frame += 1;
        if self.catalog_epoch != Some(catalog_epoch) {
            self.catalog_epoch = Some(catalog_epoch);
            self.failed.clear();
            self.failed_order.clear();
        }
        self.previews.drain(&self.ctx, self.frame);
        while let Ok(done) = self.pool.results.try_recv() {
            lock(&self.pool.shared.0).held.remove(&done.key);
            match done.outcome {
                Ok(decoded) => self.upload(done.key, decoded),
                Err(error) => {
                    eprintln!("thumbnail {}: {error}", done.key.image_id);
                    self.note_failed(done.key);
                }
            }
        }
    }

    /// End of a UI frame: queue what was asked for (latest wins) and evict by budget.
    pub fn end_frame(&mut self) {
        let jobs: Vec<ThumbJob> = std::mem::take(&mut self.asked)
            .into_iter()
            .filter(|job| !self.residents.contains(&job.key) && !self.failed.contains(&job.key))
            .collect();
        self.asked_keys.clear();
        let (queue, cvar) = &*self.pool.shared;
        lock(queue).replace(jobs);
        cvar.notify_all();
        self.residents.evict(self.frame, self.budget);
    }

    /// Stop and join the workers. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.previews.shutdown();
        self.pool.shutdown()
    }

    fn upload(&mut self, key: ThumbKey, decoded: Decoded) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [decoded.size[0] as usize, decoded.size[1] as usize],
            &decoded.rgba,
        );
        let name = format!("thumb-{}", key.image_id);
        let handle = self
            .ctx
            .load_texture(name, image, egui::TextureOptions::LINEAR);
        self.residents.insert(key, handle, decoded.size, self.frame);
    }

    fn note_failed(&mut self, key: ThumbKey) {
        if self.failed.insert(key.clone()) {
            self.failed_order.push_back(key);
        }
        while self.failed.len() > FAILED_CAP {
            match self.failed_order.pop_front() {
                Some(oldest) => self.failed.remove(&oldest),
                None => break,
            };
        }
    }
}

impl ThumbSource for ThumbStore {
    fn file_preview(&mut self, path: &str) -> ThumbState {
        self.previews.ask(path, self.frame)
    }

    fn thumb(&mut self, image: &core_model::catalog::ImageRow) -> ThumbState {
        let key = ThumbKey {
            image_id: image.id,
            hash: image.content_hash.to_ascii_lowercase(),
            edited_at: image.edited_at,
        };
        if let Some((handle, size)) = self.residents.touch(&key, self.frame) {
            return ThumbState::Ready {
                texture: handle.id(),
                size,
            };
        }
        if self.failed.contains(&key) {
            return ThumbState::Failed;
        }
        if self.asked.len() < ASKED_CAP && self.asked_keys.insert(key.clone()) {
            self.asked.push(ThumbJob {
                key: key.clone(),
                path: PathBuf::from(&image.path),
            });
        }
        match self.residents.touch_previous(&key, self.frame) {
            Some((handle, size)) => ThumbState::Ready {
                texture: handle.id(),
                size,
            },
            None => ThumbState::Loading,
        }
    }
}

enum Preview {
    Ready {
        handle: egui::TextureHandle,
        size: [u32; 2],
    },
    Failed,
}

type PreviewResult = (String, Result<Decoded, String>);

struct PreviewWorker {
    jobs: Option<Sender<String>>,
    results: Receiver<PreviewResult>,
    thread: Option<JoinHandle<()>>,
}

/// Embedded-preview decodes for not-yet-imported files: one lazily started thread, a small
/// recency-bounded texture cache keyed by path, no catalog or thumbnail-cache access.
struct Previews {
    entries: HashMap<String, (Preview, u64)>,
    pending: HashSet<String>,
    worker: Option<PreviewWorker>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Previews {
    fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            entries: HashMap::new(),
            pending: HashSet::new(),
            worker: None,
            wake,
        }
    }

    fn ask(&mut self, path: &str, frame: u64) -> ThumbState {
        if let Some((preview, last_used)) = self.entries.get_mut(path) {
            *last_used = frame;
            return match preview {
                Preview::Ready { handle, size } => ThumbState::Ready {
                    texture: handle.id(),
                    size: *size,
                },
                Preview::Failed => ThumbState::Failed,
            };
        }
        if self.pending.contains(path) {
            return ThumbState::Loading;
        }
        if self.pending.len() >= PREVIEW_INFLIGHT {
            return ThumbState::Loading;
        }
        match self.send(path) {
            Ok(()) => {
                self.pending.insert(path.to_string());
                ThumbState::Loading
            }
            Err(error) => {
                eprintln!("import preview unavailable: {error}");
                self.entries
                    .insert(path.to_string(), (Preview::Failed, frame));
                self.evict(frame);
                ThumbState::Failed
            }
        }
    }

    fn send(&mut self, path: &str) -> Result<(), String> {
        if self.worker.is_none() {
            let (jobs_tx, jobs_rx) = mpsc::channel::<String>();
            let (results_tx, results) = mpsc::channel::<PreviewResult>();
            let wake = Arc::clone(&self.wake);
            let thread = std::thread::Builder::new()
                .name("import-preview".into())
                .spawn(move || {
                    while let Ok(path) = jobs_rx.recv() {
                        let outcome = std::panic::catch_unwind(|| decode_file_preview(&path))
                            .unwrap_or_else(|payload| {
                                Err(format!(
                                    "preview worker panicked: {}",
                                    crate::photo_worker::panic_text(&*payload)
                                ))
                            });
                        if results_tx.send((path, outcome)).is_err() {
                            return;
                        }
                        wake();
                    }
                })
                .map_err(|error| format!("could not start the preview worker: {error}"))?;
            self.worker = Some(PreviewWorker {
                jobs: Some(jobs_tx),
                results,
                thread: Some(thread),
            });
        }
        self.worker
            .as_ref()
            .and_then(|worker| worker.jobs.as_ref())
            .ok_or_else(|| "preview worker stopped".to_string())?
            .send(path.to_string())
            .map_err(|_| "preview worker stopped".to_string())
    }

    fn drain(&mut self, ctx: &egui::Context, frame: u64) {
        let Some(worker) = &self.worker else { return };
        let done: Vec<PreviewResult> = worker.results.try_iter().collect();
        for (path, outcome) in done {
            self.pending.remove(&path);
            let preview = match outcome {
                Ok(decoded) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [decoded.size[0] as usize, decoded.size[1] as usize],
                        &decoded.rgba,
                    );
                    let handle =
                        ctx.load_texture("import-preview", image, egui::TextureOptions::LINEAR);
                    Preview::Ready {
                        handle,
                        size: decoded.size,
                    }
                }
                Err(error) => {
                    eprintln!("import preview {path}: {error}");
                    Preview::Failed
                }
            };
            self.entries.insert(path, (preview, frame));
        }
        self.evict(frame);
    }

    /// Drop the least recently asked-for entries beyond the cap (never the current frame's).
    fn evict(&mut self, frame: u64) {
        while self.entries.len() > PREVIEW_CAP {
            let victim = self
                .entries
                .iter()
                .filter(|(_, (_, last_used))| *last_used < frame)
                .min_by_key(|(_, (_, last_used))| *last_used)
                .map(|(path, _)| path.clone());
            match victim {
                Some(path) => self.entries.remove(&path),
                None => break,
            };
        }
    }

    fn shutdown(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.jobs = None; // closes the channel; the thread exits after its current decode
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
        self.pending.clear();
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Decode only the embedded preview of one source file; never writes to disk or the catalog.
fn decode_file_preview(path: &str) -> Result<Decoded, String> {
    let source = core_raw::source_from_path(std::path::Path::new(path))
        .map_err(|error| format!("could not open {path}: {error}"))?;
    let thumb = core_raw::thumbnail_jpeg(&source, PREVIEW_EDGE, 82)
        .map_err(|error| format!("could not decode {path}: {error}"))?;
    decode(&thumb.jpeg)
}

/// Cache tiers in legacy `protocol.rs` order; an unreadable or undecodable tier is a miss.
fn read_cached(cache: &ThumbCache, pv: i64, key: &ThumbKey) -> Option<Decoded> {
    let tiers = [
        key.edited_at
            .and_then(|version| cache.read_edited(&key.hash, pv, version).ok()),
        cache.read_canonical(&key.hash, pv).ok(),
        cache.read(&key.hash, THUMB_SIZE).ok(),
    ];
    tiers
        .into_iter()
        .flatten()
        .find_map(|jpeg| decode(&jpeg).ok())
}

fn decode(jpeg: &[u8]) -> Result<Decoded, String> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .map_err(|error| format!("undecodable thumbnail: {error}"))?
        .to_rgba8();
    Ok(Decoded {
        size: [image.width(), image.height()],
        rgba: image.into_raw(),
    })
}

/// Worker body: cache tiers, else generate the plain thumbnail from the hash-verified source.
pub fn load(cache: &ThumbCache, pv: i64, job: &ThumbJob) -> Result<Decoded, String> {
    if let Some(decoded) = read_cached(cache, pv, &job.key) {
        return Ok(decoded);
    }
    let bytes = std::fs::read(&job.path)
        .map_err(|error| format!("could not read {}: {error}", job.path.display()))?;
    if core_raw::hex(&core_raw::content_hash(&bytes)) != job.key.hash {
        return Err(format!(
            "{} no longer matches the catalogued content",
            job.path.display()
        ));
    }
    let source = core_raw::source_from_bytes(Arc::new(bytes), &job.path);
    let thumb = core_raw::thumbnail_jpeg(&source, THUMB_SIZE, 82)
        .map_err(|error| format!("could not decode {}: {error}", job.path.display()))?;
    if let Err(error) = cache.write(&job.key.hash, THUMB_SIZE, &thumb.jpeg) {
        eprintln!("could not cache thumbnail {}: {error}", job.key.hash);
    }
    decode(&thumb.jpeg)
}
