//! Renders edited thumbnails through the GPU develop pipeline into the cache's edited tier.
//!
//! The thumbnail store serves the canonical/plain thumbnail while an edit has no edited-tier file
//! yet and asks this service to produce it. One background thread drains a deduplicated, bounded
//! queue (newest request first: what the user just saved matters more than the backlog), renders
//! each job exactly as the legacy `develop_regen_thumb` did, writes
//! `cache.write_edited(hash, pv, version, jpeg)` and reports back so the store re-reads the tier.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::photo_worker::{panic_text, unsupported_reason};
use crate::thumbs::ThumbKey;
use core_library::ThumbCache;
use core_model::document::{EditDocument, ReadStatus};
use core_pipeline::{DevelopPipeline, GpuContext};
use core_runtime::catalog::CatalogAdapter;

/// Longest edge of an edited thumbnail and its JPEG quality (legacy `THUMB_EDIT_*`).
const EDIT_EDGE: u32 = 1024;
const EDIT_QUALITY: u8 = 85;
/// Jobs waiting at once; beyond this the oldest request is dropped (the store asks again if its
/// cell is still on screen).
pub const PENDING_CAP: usize = 256;

#[derive(Clone, Debug)]
pub struct EditedJob {
    /// `edited_at` is always `Some`: the edit version to render.
    pub key: ThumbKey,
    pub path: PathBuf,
    /// Catalog the key was read from; a replaced catalog invalidates the job.
    pub catalog_generation: u64,
}

pub enum Rendered {
    Written,
    /// The job no longer names the current edit or catalog; a newer key supersedes it, so nothing
    /// is reported (reporting would make the store re-ask the same missing tier).
    Dropped,
}

pub enum RenderEvent {
    Done(ThumbKey),
    Failed(ThumbKey, String),
}

/// One job's work; the production body renders on the GPU, tests inject their own.
pub type RenderBody = dyn Fn(&EditedJob) -> Result<Rendered, String> + Send + Sync + 'static;

#[derive(Default)]
struct Queue {
    pending: VecDeque<EditedJob>,
    /// Queued or running: a key in here is never queued twice.
    known: HashSet<ThumbKey>,
    stop: bool,
}

impl Queue {
    fn ensure(&mut self, job: EditedJob) {
        if !self.known.insert(job.key.clone()) {
            return;
        }
        self.pending.push_back(job);
        while self.pending.len() > PENDING_CAP {
            if let Some(oldest) = self.pending.pop_front() {
                self.known.remove(&oldest.key);
            }
        }
    }

    fn pop_newest(&mut self) -> Option<EditedJob> {
        self.pending.pop_back()
    }
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

fn lock(queue: &Mutex<Queue>) -> MutexGuard<'_, Queue> {
    queue.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct EditedThumbRenderer {
    shared: Shared,
    results: Receiver<RenderEvent>,
    thread: Option<JoinHandle<()>>,
    abandon: Arc<AtomicBool>,
    generation: Box<dyn Fn() -> u64 + Send + Sync>,
}

/// What the production body needs besides the cache.
pub struct RenderDeps {
    pub catalog: Arc<CatalogAdapter>,
    pub ctx: GpuContext,
    pub pipeline: Arc<DevelopPipeline>,
}

impl EditedThumbRenderer {
    /// `generation` names the open catalog when a job is asked for.
    #[cfg(test)]
    pub fn start(
        body: Arc<RenderBody>,
        generation: Box<dyn Fn() -> u64 + Send + Sync>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        Self::start_with_abandon(body, Arc::new(AtomicBool::new(false)), generation, wake)
    }

    pub fn production(
        deps: RenderDeps,
        cache: ThumbCache,
        process_version: i64,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let abandon = Arc::new(AtomicBool::new(false));
        let catalog = Arc::clone(&deps.catalog);
        let stopped = Arc::clone(&abandon);
        let body: Arc<RenderBody> =
            Arc::new(move |job| render_job(&deps, &cache, process_version, &stopped, job));
        let generation = Box::new(move || catalog.shared().map_or(0, |(generation, _)| generation));
        Self::start_with_abandon(body, abandon, generation, wake)
    }

    fn start_with_abandon(
        body: Arc<RenderBody>,
        abandon: Arc<AtomicBool>,
        generation: Box<dyn Fn() -> u64 + Send + Sync>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let shared: Shared = Arc::default();
        let (tx, results) = mpsc::channel();
        let (worker_shared, worker_abandon) = (Arc::clone(&shared), Arc::clone(&abandon));
        let thread = std::thread::Builder::new()
            .name("thumb-render".into())
            .spawn(move || run(&worker_shared, &tx, &*body, &worker_abandon, &*wake))
            .map_err(|error| format!("could not start the thumbnail renderer: {error}"))?;
        Ok(Self {
            shared,
            results,
            thread: Some(thread),
            abandon,
            generation,
        })
    }

    /// Ask for the edited tier of `key` (its `edited_at` must be `Some`). Idempotent while the key
    /// is queued or running.
    pub fn ensure(&self, key: ThumbKey, path: PathBuf) {
        if key.edited_at.is_none() {
            return;
        }
        let job = EditedJob {
            key,
            path,
            catalog_generation: (self.generation)(),
        };
        let (queue, cvar) = &*self.shared;
        lock(queue).ensure(job);
        cvar.notify_one();
    }

    pub fn drain(&self) -> Vec<RenderEvent> {
        self.results.try_iter().collect()
    }

    /// Stop and join the thread; a job in flight finishes its current stage and is not reported.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.abandon.store(true, Ordering::SeqCst);
        lock(&self.shared.0).stop = true;
        self.shared.1.notify_all();
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err("the thumbnail renderer thread panicked".into()),
            _ => Ok(()),
        }
    }
}

impl Drop for EditedThumbRenderer {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("{error}");
        }
    }
}

fn run(
    shared: &(Mutex<Queue>, Condvar),
    tx: &Sender<RenderEvent>,
    body: &RenderBody,
    abandon: &AtomicBool,
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
                if let Some(job) = guard.pop_newest() {
                    break job;
                }
                guard = cvar.wait(guard).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&job)))
            .unwrap_or_else(|payload| {
                Err(format!(
                    "thumbnail renderer panicked: {}",
                    panic_text(&*payload)
                ))
            });
        if abandon.load(Ordering::SeqCst) {
            return;
        }
        lock(queue).known.remove(&job.key);
        let event = match outcome {
            Ok(Rendered::Written) => RenderEvent::Done(job.key),
            Ok(Rendered::Dropped) => continue,
            Err(error) => RenderEvent::Failed(job.key, error),
        };
        if tx.send(event).is_err() {
            return; // host gone
        }
        wake();
    }
}

fn render_job(
    deps: &RenderDeps,
    cache: &ThumbCache,
    process_version: i64,
    abandon: &AtomicBool,
    job: &EditedJob,
) -> Result<Rendered, String> {
    let Some(version) = job.key.edited_at else {
        return Ok(Rendered::Dropped);
    };
    let Ok((generation, database)) = deps.catalog.shared() else {
        return Ok(Rendered::Dropped);
    };
    if generation != job.catalog_generation || !deps.catalog.is_current(generation) {
        return Ok(Rendered::Dropped);
    }
    let stored = {
        let db = database.lock().unwrap_or_else(PoisonError::into_inner);
        core_library::edits::get_edit_document_row(&db.conn, job.key.image_id)
            .map_err(|error| format!("could not read the edit: {error}"))?
    };
    let Some((json, stored_version, _)) = stored.filter(|row| row.2 == version) else {
        return Ok(Rendered::Dropped);
    };
    let document = EditDocument::read(json, stored_version);
    let mut params = match (document.status(), document.preview()) {
        (ReadStatus::Supported, Some(params)) => params.clone(),
        _ => return Err("the stored edit cannot be interpreted by this build".into()),
    };
    if let Some(reason) = unsupported_reason(&params) {
        return Err(format!("{reason} edits are not rendered by this host yet"));
    }
    if abandon.load(Ordering::SeqCst) {
        return Ok(Rendered::Dropped);
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
    let linear = core_raw::develop_linear(&source)
        .map_err(|error| format!("could not decode {}: {error}", job.path.display()))?;
    params.display_referred = core_raw::is_display(Path::new(&job.path));
    if abandon.load(Ordering::SeqCst) {
        return Ok(Rendered::Dropped);
    }
    let source_edge = params.crop.preview_source_edge(
        linear.width,
        linear.height,
        EDIT_EDGE,
        deps.ctx.max_texture_dim,
    );
    let linear = linear.downscale_into_hq(source_edge);
    let (rgba, width, height) = deps
        .pipeline
        .render_cropped_with_ai_coverages(&deps.ctx, &linear, &params, &[])
        .map_err(|error| error.to_string())?;
    let (rgba, width, height) = if width.max(height) > EDIT_EDGE {
        core_pipeline::resize_rgba8(&rgba, width, height, EDIT_EDGE)
    } else {
        (rgba, width, height)
    };
    let jpeg = core_pipeline::rgba8_to_jpeg(&rgba, width, height, EDIT_QUALITY)
        .map_err(|error| error.to_string())?;
    cache
        .write_edited(&job.key.hash, process_version, version, &jpeg)
        .map_err(|error| format!("could not cache the edited thumbnail: {error}"))?;
    Ok(Rendered::Written)
}

#[cfg(test)]
#[path = "thumb_render/tests.rs"]
mod tests;
