//! Dedicated photo decode/develop worker on the host's shared GPU device.
//!
//! One owned thread holds an `Arc<render_wgpu::Renderer>` built from the host's `GpuContext` and
//! `DevelopPipeline` (same `wgpu::Device`/`Queue` — no second device, no cross-device copies).
//! Decode (`core_raw::source_from_path` + `develop_linear`) and `prepare` NEVER run on the UI
//! thread; the worker turns a `PhotoRequest` into a `PhotoResult::Frame` lease published over an
//! mpsc channel, after which the host is woken through its event-loop proxy.
//!
//! Latest-wins intake: at most one unstarted request is queued; a newer request replaces it, so a
//! burst of selection/resize events costs at most one in-flight render plus the newest pending one.
//! Results for superseded requests are dropped by the `Presenter` on the UI side, never rendered
//! here. Shutdown sets a stop flag, notifies, and joins — no detached threads. Supersession and
//! shutdown are checked only at stage boundaries (before/after decode, develop, prepare, and in
//! every pool-wait iteration): a native decode or a GPU submission already in progress cannot be
//! interrupted and finishes its current step.
//!
//! SDR slice only: the worker refuses edits whose params need engines this host does not run yet
//! (AI denoise, AI/SAM mask coverages) — see [`unsupported_reason`]. HDR display is out of scope.
//! `read_back` is never called here; that is the explicit, test/diagnostic path on `Renderer`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use core_model::identity::{DeviceEpoch, FrameKey, QualityTier};
use core_pipeline::backend::PreparedImage;
use core_pipeline::{ComponentKind, DevelopParams, DevelopPipeline, GpuContext, ViewParams};
use render_wgpu::{FrameLease, PoolLimits, Renderer};

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

struct Slot {
    request: Option<PhotoRequest>,
    stop: bool,
}

/// Nominal byte budget for prepared GPU images. One 32 MP image incl. its lazily allocated mask
/// array (~2.0 GB nominal) fits; two (~4.1 GB) do not, so the second prepare evicts the first. A
/// 60 MP image (~3.8 GB nominal incl. masks) exceeds it: it opens alone and flags `over_budget`,
/// as does a huge panorama; the GPU OOM error scope in `prepare` reports real failure. Nominal
/// allocation arithmetic, not measured memory.
pub(crate) const PREPARED_BUDGET_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Most prepared images kept regardless of budget.
const PREPARED_MAX_ENTRIES: usize = 2;

/// Payload with a (live) nominal resident byte size.
pub(crate) trait SizedPayload {
    fn bytes(&self) -> u64;
}

impl SizedPayload for PreparedImage {
    fn bytes(&self) -> u64 {
        self.resident_bytes().total()
    }
}

/// Wall-clock milliseconds each stage of the last request took. Reset at the start of every
/// `develop` and published after the request, so a failed/abandoned request still reports the
/// stages it completed (the rest stay 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageTimings {
    /// `core_raw::source_from_path`.
    pub read_ms: u32,
    /// `core_raw::develop_linear`.
    pub develop_ms: u32,
    /// `pipeline.prepare`.
    pub prepare_ms: u32,
    /// The `render_until_free(...)` call.
    pub render_ms: u32,
    /// The `histogram_result` call.
    pub histogram_ms: u32,
    /// `prepare_image` returned a cached entry.
    pub prepared_cache_hit: bool,
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// Nominal resource numbers (allocation arithmetic, not measured memory) published by the worker
/// after each request. Pool numbers come from `Renderer::resource_report`; prepared numbers are
/// summed separately and are NOT folded into the pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceReport {
    pub prepared_gpu_bytes: u64,
    pub prepared_count: usize,
    pub pool_bytes: u64,
    pub pool_limit_bytes: u64,
    /// CPU bytes of the most recent transient decoded linear image (freed after upload).
    pub last_decode_cpu_bytes: u64,
    pub peak_decode_cpu_bytes: u64,
    /// The last prepare needed more than the budget even after evicting everything else.
    pub over_budget: bool,
    /// Stage timings of the last request (zeros before the first).
    pub last_timings: StageTimings,
}

/// Prepared-upload cache keyed by `(ImageId, content_hash)`, bounded by BYTES (and a small entry
/// cap). LRU: front of `order` is least recent; `get` refreshes. Callers run [`Self::make_room`]
/// BEFORE the expensive prepare. Generic over the payload so unit tests need no GPU.
pub(crate) struct PreparedCache<V = PreparedImage> {
    entries: HashMap<(i64, String), Arc<V>>,
    order: Vec<(i64, String)>,
    budget_bytes: u64,
    max_entries: usize,
    over_budget: bool,
    last_decode_cpu_bytes: u64,
    peak_decode_cpu_bytes: u64,
    /// Stage timings of the request in flight; reset at the start of `develop`, published by
    /// `report` (same pattern as `note_decode`).
    timings: StageTimings,
    /// Results the runner wants delivered right after the one it returns (a frame's histogram).
    follow_ups: Vec<PhotoResult>,
}

impl<V> PreparedCache<V> {
    fn new() -> Self {
        Self::with_limits(PREPARED_BUDGET_BYTES, PREPARED_MAX_ENTRIES)
    }

    fn with_limits(budget_bytes: u64, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            budget_bytes,
            max_entries,
            over_budget: false,
            last_decode_cpu_bytes: 0,
            peak_decode_cpu_bytes: 0,
            timings: StageTimings::default(),
            follow_ups: Vec::new(),
        }
    }

    /// Lookup that refreshes recency.
    fn get(&mut self, key: &(i64, String)) -> Option<&Arc<V>> {
        if self.entries.contains_key(key) {
            self.touch(key);
        }
        self.entries.get(key)
    }

    fn touch(&mut self, key: &(i64, String)) {
        if let Some(at) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(at);
            self.order.push(k);
        }
    }

    fn evict_lru(&mut self, keep: Option<&(i64, String)>) -> bool {
        let Some(at) = self.order.iter().position(|k| Some(k) != keep) else {
            return false;
        };
        let oldest = self.order.remove(at);
        self.entries.remove(&oldest);
        true
    }

    fn put(&mut self, key: (i64, String), prepared: Arc<V>) {
        if self.entries.insert(key.clone(), prepared).is_none() {
            self.order.push(key.clone());
        } else {
            self.touch(&key);
        }
        while self.order.len() > self.max_entries && self.evict_lru(Some(&key)) {}
    }

    fn note_decode(&mut self, cpu_bytes: u64) {
        self.last_decode_cpu_bytes = cpu_bytes;
        self.peak_decode_cpu_bytes = self.peak_decode_cpu_bytes.max(cpu_bytes);
    }

    fn report(&self, pool: render_wgpu::ResourceReport) -> ResourceReport
    where
        V: SizedPayload,
    {
        ResourceReport {
            prepared_gpu_bytes: self.resident_bytes(),
            prepared_count: self.entries.len(),
            pool_bytes: pool.pool_bytes,
            pool_limit_bytes: pool.pool_limit_bytes,
            last_decode_cpu_bytes: self.last_decode_cpu_bytes,
            peak_decode_cpu_bytes: self.peak_decode_cpu_bytes,
            over_budget: self.over_budget,
            last_timings: self.timings,
        }
    }
}

impl<V: SizedPayload> PreparedCache<V> {
    fn resident_bytes(&self) -> u64 {
        self.entries.values().map(|v| v.bytes()).sum()
    }

    /// Evict LRU entries (never `keep`) until `incoming` more bytes fit the budget and a new entry
    /// fits the entry cap, or nothing evictable is left. An `incoming` that cannot fit even then
    /// still proceeds (a lone huge image must open) and sets `over_budget`.
    fn make_room(&mut self, incoming: u64, keep: Option<&(i64, String)>) {
        while (self.resident_bytes().saturating_add(incoming) > self.budget_bytes
            || self.order.len() >= self.max_entries)
            && self.evict_lru(keep)
        {}
        self.over_budget = self.resident_bytes().saturating_add(incoming) > self.budget_bytes;
    }
}

/// "Should this request be dropped?" — stop flag set or a newer request queued (latest-wins).
type Abandon<'a> = &'a dyn Fn() -> bool;

/// The worker's runnable unit; the real one decodes + develops, unit tests inject a stub.
type Runner = dyn FnMut(PhotoRequest, &mut PreparedCache, Abandon<'_>) -> Option<PhotoResult>
    + Send
    + 'static;

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
    slot: Arc<(Mutex<Slot>, Condvar)>,
    results: Receiver<PhotoResult>,
    thread: Option<JoinHandle<()>>,
    stopped: Option<String>,
    report: Arc<Mutex<ResourceReport>>,
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
        let runner =
            move |request: PhotoRequest, cache: &mut PreparedCache, abandon: Abandon<'_>| {
                let result = develop(&worker_ctx, &pipeline, &renderer, request, cache, abandon);
                *published
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    cache.report(renderer.resource_report());
                result
            };
        let mut worker = Self::with_runner(Box::new(runner), wake)?;
        worker.report = report;
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
                stop: false,
            }),
            Condvar::new(),
        ));
        let (tx, results) = mpsc::channel();
        let body = {
            let slot = Arc::clone(&slot);
            Box::new(move || worker_loop(&slot, &tx, runner.as_mut(), &*wake))
        };
        let thread =
            spawn(body).map_err(|e| format!("could not start photo worker thread: {e}"))?;
        Ok(Self {
            slot,
            results,
            thread: Some(thread),
            stopped: None,
            report: Arc::new(Mutex::new(ResourceReport::default())),
        })
    }

    /// Latest-wins intake: replaces an unstarted request; a request already in flight is
    /// abandoned at its next stage boundary. A dead worker ignores requests.
    pub fn request(&self, request: PhotoRequest) {
        let (lock, cvar) = &*self.slot;
        let mut guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.request = Some(request);
        cvar.notify_one();
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
        self.slot
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop
    }

    /// Stop and join. Stage boundaries and pool waits observe the stop flag, so this returns
    /// within about one `POOL_RETRY_SLEEP` unless a native decode/GPU step is in progress.
    /// `Err` when the worker thread had panicked. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), String> {
        let (lock, cvar) = &*self.slot;
        lock.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop = true;
        cvar.notify_all();
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(payload)) => Err(format!("photo worker panicked: {}", panic_text(&*payload))),
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

pub(crate) fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string())
}

fn worker_loop(
    slot: &(Mutex<Slot>, Condvar),
    tx: &Sender<PhotoResult>,
    runner: &mut Runner,
    wake: &(dyn Fn() + Send + Sync),
) {
    let (lock, cvar) = slot;
    let abandon = || {
        let guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.stop || guard.request.is_some()
    };
    let mut cache = PreparedCache::new();
    loop {
        let request = {
            let mut guard = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if guard.stop {
                    return;
                }
                if let Some(request) = guard.request.take() {
                    break request;
                }
                guard = cvar
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runner(request, &mut cache, &abandon)
        }));
        match ran {
            Ok(Some(result)) => {
                let silent = matches!(result, PhotoResult::Abandoned(_));
                if tx.send(result).is_err() {
                    return; // host gone; stop rather than develop into a dead channel
                }
                for follow_up in cache.follow_ups.drain(..) {
                    if tx.send(follow_up).is_err() {
                        return;
                    }
                }
                // The frame or its message only becomes visible on a redraw; wake the loop.
                if !silent {
                    wake();
                }
            }
            Ok(None) => return, // the runner observed shutdown; stop
            Err(payload) => {
                let _ = tx.send(PhotoResult::WorkerDied(format!(
                    "photo worker panicked: {}",
                    panic_text(&*payload)
                )));
                wake();
                return;
            }
        }
    }
}

/// Pool exhaustion is transient (every target of this size leased, e.g. while the presenter still
/// holds the previous frame): bounded backoff before surfacing a message.
const POOL_RETRY_LIMIT: u32 = 250;
const POOL_RETRY_SLEEP: Duration = Duration::from_millis(20);

/// Why [`render_until_free`] gave up.
#[derive(Debug, PartialEq, Eq)]
enum RenderStop {
    Abandoned,
    Busy,
    Failed(String),
}

/// Retry `attempt` while the pool is exhausted, checking `abandon` before every attempt (so a
/// superseded or shutting-down request leaves within one `POOL_RETRY_SLEEP`).
fn render_until_free<T>(
    abandon: Abandon<'_>,
    mut attempt: impl FnMut() -> Result<T, render_wgpu::RenderError>,
) -> Result<T, RenderStop> {
    for tries in 1..=POOL_RETRY_LIMIT {
        if abandon() {
            return Err(RenderStop::Abandoned);
        }
        match attempt() {
            Ok(value) => return Ok(value),
            Err(render_wgpu::RenderError::PoolExhausted { .. }) => {
                if tries == POOL_RETRY_LIMIT {
                    break;
                }
                std::thread::sleep(POOL_RETRY_SLEEP);
            }
            Err(error) => return Err(RenderStop::Failed(format!("develop failed: {error}"))),
        }
    }
    Err(RenderStop::Busy)
}

/// Decode + upload `path` (cached by `cache_key`), checking `abandon` between stages. `Ok(None)`
/// = abandoned. The prepared image is cached even when abandoned right after: it is valid work.
fn prepare_image(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    request: &PhotoRequest,
    cache: &mut PreparedCache,
    abandon: Abandon<'_>,
) -> Result<Option<Arc<PreparedImage>>, String> {
    let cache_key = (
        request.key.identity.source.image_id.0,
        request.content_hash.clone(),
    );
    let hit = cache.get(&cache_key).cloned();
    if let Some(prepared) = hit {
        cache.timings.prepared_cache_hit = true;
        return Ok(Some(prepared));
    }
    let path = &request.path;
    let started = Instant::now();
    let source = core_raw::source_from_path(path).map_err(|error| {
        crate::presence::missing_source_message(path, &error)
            .unwrap_or_else(|| format!("could not read {}: {error}", path.display()))
    })?;
    cache.timings.read_ms = elapsed_ms(started);
    if abandon() {
        return Ok(None);
    }
    let started = Instant::now();
    let linear = core_raw::develop_linear(&source)
        .map_err(|error| format!("could not develop {}: {error}", path.display()))?;
    cache.timings.develop_ms = elapsed_ms(started);
    if abandon() {
        return Ok(None);
    }
    cache.note_decode(std::mem::size_of_val(linear.data.as_slice()) as u64);
    // Evict BEFORE the expensive upload. Masks may be allocated lazily later, so reserve them.
    let (w, h) = (linear.width, linear.height);
    cache.make_room(
        core_pipeline::backend::estimate_prepare_bytes(w, h)
            + core_pipeline::backend::estimate_mask_bytes(w, h),
        None,
    );
    let started = Instant::now();
    let prepared = pipeline
        .prepare(ctx, &linear)
        .map_err(|error| format!("GPU prepare failed: {error}"))?;
    cache.timings.prepare_ms = elapsed_ms(started);
    let prepared = Arc::new(prepared);
    cache.put(cache_key, Arc::clone(&prepared));
    Ok(Some(prepared))
}

/// Edge of the square render the histogram is counted over (legacy used 384; 256 is plenty for
/// 256 bins and keeps the extra readback tiny).
const HIST_RENDER_EDGE: u32 = 256;

/// Whole-crop histogram of the frame just rendered. Failure is silent: the histogram is a
/// readout, never a reason to fail the frame.
fn histogram_result(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    key: FrameKey,
    prepared: &PreparedImage,
    params: &DevelopParams,
) -> Option<PhotoResult> {
    let view = ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w: HIST_RENDER_EDGE,
        out_h: HIST_RENDER_EDGE,
        active: true,
        overlay_layer: -1,
        overlay_color: [0.0, 0.0, 0.0],
        overlay_strength: 0.0,
        crop_preview: false,
    };
    match pipeline.render_view(ctx, prepared, params, &view) {
        Ok(rgba) => {
            let h = core_pipeline::histogram(&rgba);
            Some(PhotoResult::Histogram {
                key,
                r: h.r,
                g: h.g,
                b: h.b,
            })
        }
        Err(error) => {
            eprintln!("histogram render failed: {error}");
            None
        }
    }
}

/// Queue the histogram follow-up and time the whole thing (the GPU work dominates the queueing).
fn add_histogram(
    cache: &mut PreparedCache,
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    key: FrameKey,
    prepared: &PreparedImage,
    params: &DevelopParams,
) {
    let started = Instant::now();
    cache
        .follow_ups
        .extend(histogram_result(ctx, pipeline, key, prepared, params));
    cache.timings.histogram_ms = elapsed_ms(started);
}

/// Decode (or reuse the cached upload) + develop into a leased target. Worker thread only.
fn develop(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    renderer: &Renderer,
    request: PhotoRequest,
    cache: &mut PreparedCache,
    abandon: Abandon<'_>,
) -> Option<PhotoResult> {
    let key = request.key;
    cache.timings = StageTimings::default();
    let failed = |message: String| Some(PhotoResult::Failed { key, message });
    if let Some(feature) = unsupported_reason(&request.params) {
        return failed(format!(
            "Darkroom can't render this photo yet: {feature} is not supported by this build — \
             nothing was changed."
        ));
    }
    if abandon() {
        return Some(PhotoResult::Abandoned(key));
    }
    // Intrinsic to the file, derived here — never trust a persisted `display_referred`
    // (legacy `decode_develop` does exactly this).
    let mut params = (*request.params).clone();
    params.display_referred = core_raw::is_display(&request.path);

    let prepared = match prepare_image(ctx, pipeline, &request, cache, abandon) {
        Ok(Some(prepared)) => prepared,
        Ok(None) => return Some(PhotoResult::Abandoned(key)),
        Err(message) => return failed(message),
    };
    let started = Instant::now();
    let rendered = render_until_free(abandon, || {
        renderer.render(key, &prepared, &params, &request.view)
    });
    cache.timings.render_ms = elapsed_ms(started);
    match rendered {
        Ok(lease) => {
            // The histogram is the expensive half of a request (a second 256² render plus a
            // blocking readback) and nobody reads it mid-drag: only settled frames pay for it.
            // `timings.histogram_ms` stays 0 for the skipped tier.
            if !abandon() && key.quality == QualityTier::Settled {
                add_histogram(cache, ctx, pipeline, key, &prepared, &params);
            }
            Some(PhotoResult::Frame(lease))
        }
        Err(RenderStop::Abandoned) => Some(PhotoResult::Abandoned(key)),
        Err(RenderStop::Busy) => failed("GPU render targets are busy; try again".into()),
        Err(RenderStop::Failed(message)) => failed(message),
    }
}

#[cfg(test)]
#[path = "photo_worker/tests.rs"]
mod tests;
