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

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::presence::PresenceStore;
use crate::thumb_render::{EditedThumbRenderer, RenderDeps, RenderEvent};
use core_library::{ThumbCache, THUMB_SIZE};
use ui_egui::{Presence, ThumbSource, ThumbState};

mod previews;
mod residents;
use previews::Previews;
use residents::BoundedKeys;
pub use residents::Residents;

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
    /// An edited key was served from a non-edited tier (canonical/plain/generated): the edited
    /// render is still owed.
    pub provisional: bool,
}

struct Done {
    key: ThumbKey,
    path: PathBuf,
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
            path: job.path,
            outcome,
        };
        if tx.send(done).is_err() {
            return; // host gone
        }
        wake();
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
    presence: PresenceStore,
    renderer: Option<EditedThumbRenderer>,
    /// Edited keys already rendered or refused: a provisional result for one of them is not
    /// ensured again, so a failing render is never retried every frame.
    render_seen: BoundedKeys,
}

impl ThumbStore {
    /// `cache_root` is `<catalog dir>/thumbs`. `wake` redraws the window when a result lands.
    /// `render` enables producing edited thumbnails; without it edited keys keep showing the
    /// unedited thumbnail.
    pub fn new(
        ctx: egui::Context,
        cache_root: PathBuf,
        process_version: i64,
        wake: Arc<dyn Fn() + Send + Sync>,
        render: Option<RenderDeps>,
    ) -> Result<Self, String> {
        let cache = ThumbCache::new(cache_root)
            .map_err(|error| format!("could not open the thumbnail cache: {error}"))?;
        let renderer = render
            .map(|deps| {
                EditedThumbRenderer::production(
                    deps,
                    cache.clone(),
                    process_version,
                    Arc::clone(&wake),
                )
            })
            .transpose()?;
        let loader: Arc<Loader> = Arc::new(move |job| load(&cache, process_version, job));
        let mut store = Self::with_loader(ctx, loader, wake, THUMB_TEXTURE_BUDGET)?;
        store.renderer = renderer;
        Ok(store)
    }

    /// Test seam: attach a renderer built around an injected body.
    #[cfg(test)]
    pub fn with_renderer(mut self, renderer: EditedThumbRenderer) -> Self {
        self.renderer = Some(renderer);
        self
    }

    pub fn with_loader(
        ctx: egui::Context,
        loader: Arc<Loader>,
        wake: Arc<dyn Fn() + Send + Sync>,
        budget: u64,
    ) -> Result<Self, String> {
        let wake_for_previews = Arc::clone(&wake);
        let presence = PresenceStore::new(Arc::clone(&wake))?;
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
            presence,
            renderer: None,
            render_seen: BoundedKeys::new(FAILED_CAP),
        })
    }

    /// Start of a UI frame: upload finished thumbnails; a new catalog epoch forgets failures.
    pub fn begin_frame(&mut self, catalog_epoch: u64) {
        self.frame += 1;
        if self.catalog_epoch != Some(catalog_epoch) {
            self.catalog_epoch = Some(catalog_epoch);
            self.failed.clear();
            self.failed_order.clear();
            self.render_seen.clear();
            // Provisional textures are re-asked, so they are ensured against the new catalog.
            self.residents.remove_provisional();
            self.presence.invalidate();
        }
        self.previews.drain(&self.ctx, self.frame);
        while let Ok(done) = self.pool.results.try_recv() {
            lock(&self.pool.shared.0).held.remove(&done.key);
            match done.outcome {
                Ok(decoded) => self.upload(done.key, done.path, decoded),
                Err(error) => {
                    eprintln!("thumbnail {}: {error}", done.key.image_id);
                    self.note_failed(done.key);
                }
            }
        }
        self.drain_renders();
    }

    fn drain_renders(&mut self) {
        let Some(renderer) = &self.renderer else {
            return;
        };
        for event in renderer.drain() {
            match event {
                // The edited tier now exists: forget the provisional texture and any failure so
                // the next frame asks again and reads it.
                RenderEvent::Done(key) => {
                    self.residents.remove(&key);
                    self.failed.remove(&key);
                    self.failed_order.retain(|other| other != &key);
                }
                RenderEvent::Failed(key, error) => {
                    eprintln!("edited thumbnail {}: {error}", key.image_id);
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

    /// Files may have come or gone while the window was in the background.
    pub fn invalidate_presence(&self) {
        self.presence.invalidate();
    }

    /// Stop and join the workers. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.previews.shutdown();
        let presence = self.presence.shutdown();
        let renderer = self
            .renderer
            .as_mut()
            .map_or(Ok(()), EditedThumbRenderer::shutdown);
        self.pool.shutdown().and(presence).and(renderer)
    }

    fn upload(&mut self, key: ThumbKey, path: PathBuf, decoded: Decoded) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [decoded.size[0] as usize, decoded.size[1] as usize],
            &decoded.rgba,
        );
        let name = format!("thumb-{}", key.image_id);
        let handle = self
            .ctx
            .load_texture(name, image, egui::TextureOptions::LINEAR);
        if decoded.provisional && !self.render_seen.contains(&key) {
            if let Some(renderer) = &self.renderer {
                renderer.ensure(key.clone(), path);
                self.render_seen.insert(key.clone());
            }
        }
        self.residents
            .insert_with(key, handle, decoded.size, self.frame, decoded.provisional);
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
    fn presence(&mut self, image: &core_model::catalog::ImageRow) -> Presence {
        self.presence.ask(image.id, &image.path)
    }

    fn file_preview(&mut self, path: &str) -> ThumbState {
        self.previews.ask(path, self.frame)
    }

    fn thumb(&mut self, image: &core_model::catalog::ImageRow) -> ThumbState {
        let key = ThumbKey {
            image_id: image.id,
            hash: image.content_hash.to_ascii_lowercase(),
            edited_at: image.edited_at,
        };
        if self.residents.is_provisional(&key) {
            // Keep showing the last settled edit while this one's render is owed.
            if let Some((handle, size)) = self.residents.touch_settled_previous(&key, self.frame) {
                return ThumbState::Ready {
                    texture: handle.id(),
                    size,
                };
            }
        }
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

/// Cache tiers in legacy `protocol.rs` order; an unreadable or undecodable tier is a miss.
fn read_cached(cache: &ThumbCache, pv: i64, key: &ThumbKey) -> Option<Decoded> {
    let tiers = [
        key.edited_at
            .and_then(|version| cache.read_edited(&key.hash, pv, version).ok()),
        cache.read_canonical(&key.hash, pv).ok(),
        cache.read(&key.hash, THUMB_SIZE).ok(),
    ];
    let (tier, mut decoded) = tiers
        .into_iter()
        .enumerate()
        .find_map(|(tier, jpeg)| decode(&jpeg?).ok().map(|decoded| (tier, decoded)))?;
    decoded.provisional = key.edited_at.is_some() && tier != 0;
    Some(decoded)
}

fn decode(jpeg: &[u8]) -> Result<Decoded, String> {
    let image = image::load_from_memory_with_format(jpeg, image::ImageFormat::Jpeg)
        .map_err(|error| format!("undecodable thumbnail: {error}"))?
        .to_rgba8();
    Ok(Decoded {
        size: [image.width(), image.height()],
        rgba: image.into_raw(),
        provisional: false,
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
    let mut decoded = decode(&thumb.jpeg)?;
    decoded.provisional = job.key.edited_at.is_some();
    Ok(decoded)
}
