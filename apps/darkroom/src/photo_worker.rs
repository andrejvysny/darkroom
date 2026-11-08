//! Dedicated photo decode/develop worker on the host's shared GPU device.
//!
//! One owned thread holds an `Arc<render_wgpu::Renderer>` built from the host's `GpuContext` and
//! `DevelopPipeline` (same `wgpu::Device`/`Queue` — no second device, no cross-device copies).
//! Decode (`core_raw::source_from_path` + `develop_linear`) and `prepare` NEVER run on the UI
//! thread; the worker turns a `PhotoRequest` into a `PhotoResult::Frame` lease published over an
//! mpsc channel, after which the host is woken through its event-loop proxy.
//!
//! Intake: one render slot and one warm-up slot, both latest-wins; a render always runs before a
//! warm-up. A warm-up decodes + uploads the selected photo while its edit document is still
//! loading, so the first render finds it cached. Supersession is checked only at stage boundaries
//! (before/after decode, develop, prepare, and in every pool wait): newer work for ANOTHER photo
//! abandons a decode; newer work for the SAME photo keeps it (it needs exactly that upload) and
//! only replaces the output. A native decode or a GPU submission already in progress cannot be
//! interrupted and finishes its current step. Results for superseded requests are dropped by the
//! `Presenter` on the UI side. A settled frame's histogram is computed only after that frame was
//! delivered, and skipped when newer work waits. Shutdown sets a stop flag, notifies, and joins —
//! no detached threads.
//!
//! SDR slice only: the worker refuses edits whose params need engines this host does not run yet
//! (AI denoise, AI/SAM mask coverages) — see [`unsupported_reason`]. HDR display is out of scope.
//! `read_back` is never called here; that is the explicit, test/diagnostic path on `Renderer`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
#[cfg(test)]
use std::time::Duration;

use core_model::identity::{DeviceEpoch, FrameKey};
use core_pipeline::{ComponentKind, DevelopParams, DevelopPipeline, GpuContext, ViewParams};
use render_wgpu::{FrameLease, PoolLimits, Renderer};

mod develop;
mod inbox;
mod prefetch;
mod prepared;

#[cfg(test)]
use develop::{render_until_free, RenderStop};
pub(crate) use inbox::Ctl;
use inbox::{next_job, Slot, SlotPair};
pub use prepared::ResourceReport;
use prepared::{PreparedCache, SourceKey};
#[cfg(test)]
use prepared::{SizedPayload, StageTimings, PREPARED_BUDGET_BYTES};

/// One presentation render: develop `path`'s image with `params` under `view` into a
/// `key.viewport_px` target. `content_hash` keys the prepared cache, so a re-indexed image with
/// new bytes never reuses a stale prepared upload under the same `ImageId`.
pub struct PhotoRequest {
    pub key: FrameKey,
    pub path: PathBuf,
    pub content_hash: String,
    pub params: Arc<DevelopParams>,
    pub view: ViewParams,
}

impl PhotoRequest {
    fn source_key(&self) -> SourceKey {
        (
            self.key.identity.source.image_id.0,
            self.content_hash.clone(),
        )
    }
}

/// Decode + upload a photo ahead of its first render (see [`PhotoWorker::warm`]).
pub struct WarmRequest {
    pub image_id: i64,
    pub path: PathBuf,
    pub content_hash: String,
}

impl WarmRequest {
    fn source_key(&self) -> SourceKey {
        (self.image_id, self.content_hash.clone())
    }
}

/// The worker's answer to a `PhotoRequest`, sent to the UI thread.
pub enum PhotoResult {
    /// Developed frame, ready for `Presenter::offer`.
    Frame(FrameLease),
    /// The frame could not be produced; `message` is fit to show the user.
    Failed { key: FrameKey, message: String },
    /// The request was superseded (or shutdown began) at a stage boundary; the host ignores it.
    Abandoned(FrameKey),
    /// 256-bin histogram of the same prepared image + params as the `Frame` with this `key`
    /// (whole crop, small fixed render). Sent right after that frame; never sent on failure.
    Histogram {
        key: FrameKey,
        r: Vec<u32>,
        g: Vec<u32>,
        b: Vec<u32>,
    },
    /// Terminal: the worker thread panicked. No further results will arrive.
    WorkerDied(String),
}

/// Why an otherwise-`Ready` document must not be rendered by this host. The name is shown to the
/// user, so it must say WHICH feature, not just "unsupported".
pub fn unsupported_reason(params: &DevelopParams) -> Option<&'static str> {
    if !params.denoise.is_off() {
        return Some("AI denoise");
    }
    // Same layer set the GPU consumes (legacy `validate_ai_coverages` policy): enabled masks
    // within the cap. A disabled Ai component is never sampled, so it does not block rendering.
    let ai_mask = params
        .masks
        .iter()
        .filter(|mask| mask.enabled)
        .take(core_pipeline::MASK_CAP)
        .flat_map(|mask| mask.components.iter())
        .any(|component| matches!(component.kind, ComponentKind::Ai { .. }));
    if ai_mask {
        return Some("AI masks");
    }
    None
}

/// The legacy full-frame view constructor: whole crop, letterbox-fit into `width × height`,
/// overlay off — what an aspect-fit of the crop into the panel shows 1:1.
pub fn fit_view(width: u32, height: u32) -> ViewParams {
    ViewParams::full(width.max(1), height.max(1))
}

/// The shader viewport path: render only the crop-local window `[origin, origin + size]` into a
/// `width × height` target. Overlay defaults come from [`ViewParams::full`] — the presenter never
/// tints a mask over the presented frame (the histogram worker owns any overlay).
pub fn window_view(origin: [f32; 2], size: [f32; 2], width: u32, height: u32) -> ViewParams {
    ViewParams {
        origin,
        size,
        out_w: width.max(1),
        out_h: height.max(1),
        active: true,
        ..ViewParams::full(width, height)
    }
}

/// A job the worker runs.
pub(crate) enum Job {
    Render(PhotoRequest),
    Warm(WarmRequest),
    /// Upload a prefetched neighbour; runs only when no render/warm is queued.
    Upload(Box<prefetch::Decoded>),
    /// Follow-up of a delivered settled frame; see [`prepared::HistogramJob`].
    Histogram(Box<prepared::HistogramJob>),
}

/// The worker's runnable unit; the real one decodes + develops, unit tests inject a stub.
/// `None` from a `Render` job means the runner observed shutdown; other jobs may return `None`.
type Runner = dyn FnMut(Job, &mut PreparedCache, &Ctl<'_>) -> Option<PhotoResult> + Send + 'static;

/// Spawns the worker body; the production one is `std::thread::Builder`, tests inject a failure.
type Spawn = dyn FnOnce(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<JoinHandle<()>>;

/// Liveness of the worker thread as seen by the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerStatus {
    Running,
    /// The worker is gone (panic or unexpected exit); the string says why.
    Stopped(String),
}

pub struct PhotoWorker {
    slot: Arc<SlotPair>,
    results: Receiver<PhotoResult>,
    thread: Option<JoinHandle<()>>,
    /// Neighbour decode handoff; its thread is spawned by [`Self::start_prefetch`].
    prefetch: Arc<prefetch::Prefetch>,
    prefetch_thread: Option<JoinHandle<()>>,
    stopped: Option<String>,
    report: Arc<Mutex<ResourceReport>>,
    /// Set around every runner call; with the inbox it answers `busy_probe`.
    running: Arc<AtomicBool>,
}

impl PhotoWorker {
    /// Start the decode/develop worker sharing the host's device. `ctx`/`pipeline` are the host's
    /// own handles; the worker wraps the pipeline in its own `Renderer` over the same
    /// device/queue. `wake` must trigger a UI redraw (the winit event-loop proxy). `Err` when the
    /// thread cannot be spawned.
    pub fn start(
        ctx: &GpuContext,
        pipeline: Arc<DevelopPipeline>,
        device_epoch: DeviceEpoch,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, String> {
        let handles = || GpuContext {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            info: ctx.info.clone(),
            max_texture_dim: ctx.max_texture_dim,
        };
        let renderer = Arc::new(Renderer::new(
            handles(),
            Arc::clone(&pipeline),
            device_epoch,
            PoolLimits::default(),
        ));
        let worker_ctx = handles();
        let report = Arc::new(Mutex::new(ResourceReport::default()));
        let published = Arc::clone(&report);
        let running = Arc::new(AtomicBool::new(false));
        let job_running = Arc::clone(&running);
        let worker_renderer = Arc::clone(&renderer);
        let runner = move |job: Job, cache: &mut PreparedCache, ctl: &Ctl<'_>| {
            let is_render = matches!(job, Job::Render(_));
            job_running.store(true, Ordering::SeqCst);
            let result = develop::run(&worker_ctx, &pipeline, &worker_renderer, job, cache, ctl);
            job_running.store(false, Ordering::SeqCst);
            if is_render && crate::open_journey::enabled() {
                develop::trace_timings(&develop::outcome(&result), &cache.timings);
            }
            *published
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                cache.report(worker_renderer.resource_report());
            result
        };
        let mut worker = Self::with_runner(Box::new(runner), wake)?;
        // Losing the prefetch only costs speed; photos still open.
        if let Err(error) = worker.start_prefetch(Box::new(prefetch::decode_full)) {
            eprintln!("{error}");
        }
        worker.report = report;
        worker.running = running;
        // A released target wakes a render waiting on an exhausted pool (no polling).
        let slot = Arc::downgrade(&worker.slot);
        renderer.set_return_signal(Arc::new(move || {
            if let Some(slot) = slot.upgrade() {
                slot.0
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pool_returns += 1;
                slot.1.notify_all();
            }
        }));
        Ok(worker)
    }

    /// Latest nominal resource numbers, published after each request (default before the first).
    pub fn resource_report(&self) -> ResourceReport {
        *self
            .report
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn with_runner(runner: Box<Runner>, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        Self::with_spawn(
            runner,
            wake,
            Box::new(|body| {
                std::thread::Builder::new()
                    .name("photo-worker".into())
                    .spawn(body)
            }),
        )
    }

    fn with_spawn(
        mut runner: Box<Runner>,
        wake: Arc<dyn Fn() + Send + Sync>,
        spawn: Box<Spawn>,
    ) -> Result<Self, String> {
        let slot = Arc::new((
            Mutex::new(Slot {
                request: None,
                warm: None,
                stop: false,
                pool_returns: 0,
            }),
            Condvar::new(),
        ));
        let (tx, results) = mpsc::channel();
        let prefetch = prefetch::Prefetch::new(Arc::clone(&slot));
        let body = {
            let slot = Arc::clone(&slot);
            let prefetch = Arc::clone(&prefetch);
            Box::new(move || worker_loop(&slot, &prefetch, &tx, runner.as_mut(), &*wake))
        };
        let thread =
            spawn(body).map_err(|e| format!("could not start photo worker thread: {e}"))?;
        Ok(Self {
            slot,
            results,
            thread: Some(thread),
            prefetch,
            prefetch_thread: None,
            stopped: None,
            report: Arc::new(Mutex::new(ResourceReport::default())),
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    fn inbox(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// A probe answering whether a job is running or waiting. Cheap enough to poll from another
    /// thread; background renderers yield to the interactive path on it.
    pub fn busy_probe(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let slot = Arc::clone(&self.slot);
        let running = Arc::clone(&self.running);
        Arc::new(move || is_busy(&slot, &running))
    }

    /// Latest-wins intake: replaces an unstarted request; a request already in flight is
    /// abandoned at its next stage boundary (its decode survives if it is the same photo). A
    /// dead worker ignores requests.
    pub fn request(&self, request: PhotoRequest) {
        self.inbox().request = Some(request);
        self.slot.1.notify_all();
    }

    /// Start decoding + uploading the selected photo before its edit document is ready (latest
    /// wins; a render request always runs first). The render that follows hits the cache.
    pub fn warm(&self, request: WarmRequest) {
        self.inbox().warm = Some(request);
        self.slot.1.notify_all();
    }

    /// Non-blocking drain of finished results (UI thread, each redraw). Notes worker death:
    /// a `WorkerDied` result or a disconnected channel while no stop was requested.
    pub fn drain_results(&mut self) -> Vec<PhotoResult> {
        let mut out = Vec::new();
        loop {
            match self.results.try_recv() {
                Ok(result) => {
                    if let PhotoResult::WorkerDied(reason) = &result {
                        self.stopped.get_or_insert_with(|| reason.clone());
                    }
                    out.push(result);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.stop_requested() {
                        self.stopped.get_or_insert_with(|| {
                            "the photo worker thread exited unexpectedly".to_string()
                        });
                    }
                    break;
                }
            }
        }
        out
    }

    /// Whether the worker is alive, as of the last [`Self::drain_results`] (which is what
    /// detects a panic or disconnection; this never drains, so no result is lost).
    pub fn status(&mut self) -> WorkerStatus {
        match &self.stopped {
            Some(reason) => WorkerStatus::Stopped(reason.clone()),
            None => WorkerStatus::Running,
        }
    }

    fn stop_requested(&self) -> bool {
        self.inbox().stop
    }

    /// Stop and join. Stage boundaries and pool waits observe the stop flag, so this returns
    /// promptly unless a native decode/GPU step is in progress. `Err` when the worker thread had
    /// panicked. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), String> {
        self.inbox().stop = true;
        self.slot.1.notify_all();
        self.prefetch.stop();
        // A prefetch panic must not hide a worker one, and neither blocks the other's join.
        let prefetch = self.prefetch_thread.take().map(JoinHandle::join);
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(payload)) => Err(format!("photo worker panicked: {}", panic_text(&*payload))),
            _ if matches!(prefetch, Some(Err(_))) => Err("photo prefetch thread panicked".into()),
            _ => Ok(()),
        }
    }
}

impl Drop for PhotoWorker {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("{error}");
        }
    }
}

fn is_busy(slot: &SlotPair, running: &AtomicBool) -> bool {
    let inbox = slot
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    running.load(Ordering::SeqCst) || inbox.request.is_some() || inbox.warm.is_some()
}

pub(crate) fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

/// What one job did: `Err` is the worker-death message after a runner panic.
fn run_caught(
    runner: &mut Runner,
    job: Job,
    cache: &mut PreparedCache,
    ctl: &Ctl<'_>,
) -> Result<Option<PhotoResult>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| runner(job, cache, ctl)))
        .map_err(|payload| format!("photo worker panicked: {}", panic_text(&*payload)))
}

fn worker_loop(
    slot: &SlotPair,
    prefetch: &Arc<prefetch::Prefetch>,
    tx: &Sender<PhotoResult>,
    runner: &mut Runner,
    wake: &(dyn Fn() + Send + Sync),
) {
    let ctl = Ctl { slot };
    let mut cache = PreparedCache::new();
    cache.handoff = Some(Arc::clone(prefetch));
    while let Some(job) = next_job(slot, Some(prefetch)) {
        let is_render = matches!(job, Job::Render(_));
        let outcome = run_caught(runner, job, &mut cache, &ctl);
        // The job is over: the decode thread may refill the slot, and it must not re-decode
        // what the cache now holds.
        prefetch.release();
        prefetch.set_known(cache.known_sources());
        let result = match outcome {
            Ok(Some(result)) => result,
            // The runner observed shutdown during a render; stop.
            Ok(None) if is_render => return,
            Ok(None) => continue,
            Err(reason) => {
                let _ = tx.send(PhotoResult::WorkerDied(reason));
                wake();
                return;
            }
        };
        let silent = matches!(result, PhotoResult::Abandoned(_));
        if tx.send(result).is_err() {
            return; // host gone; stop rather than develop into a dead channel
        }
        // The frame or its message only becomes visible on a redraw; wake the loop.
        if !silent {
            wake();
        }
        // The histogram is owed only after its frame is on its way, and only if nothing newer
        // waits (a newer frame brings its own).
        let Some(histogram) = cache.histogram_due.take() else {
            continue;
        };
        if ctl.has_pending() {
            continue;
        }
        match run_caught(runner, Job::Histogram(histogram), &mut cache, &ctl) {
            Ok(Some(result)) => {
                if tx.send(result).is_err() {
                    return;
                }
                wake();
            }
            Ok(None) => {}
            Err(reason) => {
                let _ = tx.send(PhotoResult::WorkerDied(reason));
                wake();
                return;
            }
        }
    }
}

#[cfg(test)]
#[path = "photo_worker/tests.rs"]
mod tests;
