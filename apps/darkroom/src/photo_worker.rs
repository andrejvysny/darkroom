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
//! here. Shutdown sets a stop flag, notifies, and joins — no detached threads. A request already
//! in progress finishes its current GPU step; its result is dropped (send error) or rejected by
//! the presenter.
//!
//! SDR slice only: the worker refuses edits whose params need engines this host does not run yet
//! (AI denoise, AI/SAM mask coverages) — see [`unsupported_reason`]. HDR display is out of scope.
//! `read_back` is never called here; that is the explicit, test/diagnostic path on `Renderer`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use core_model::identity::{DeviceEpoch, FrameKey};
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

struct Slot {
    request: Option<PhotoRequest>,
    stop: bool,
}

/// Prepared-upload cache: at most two images, keyed by `(ImageId, content_hash)`. Selection
/// history is LRU-shaped; at this size insertion-order eviction is sufficient. Generic over the
/// payload so unit tests can exercise eviction without a GPU.
pub(crate) struct PreparedCache<V = PreparedImage> {
    entries: HashMap<(i64, String), Arc<V>>,
    order: Vec<(i64, String)>,
}

impl<V> PreparedCache<V> {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn get(&self, key: &(i64, String)) -> Option<&Arc<V>> {
        self.entries.get(key)
    }

    fn put(&mut self, key: (i64, String), prepared: Arc<V>) {
        if self.entries.insert(key.clone(), prepared).is_none() {
            self.order.push(key);
            while self.order.len() > 2 {
                let oldest = self.order.remove(0);
                self.entries.remove(&oldest);
            }
        }
    }
}

/// The worker's runnable unit; the real one decodes + develops, unit tests inject a stub.
type Runner = dyn FnMut(PhotoRequest, &mut PreparedCache) -> Option<PhotoResult> + Send + 'static;

pub struct PhotoWorker {
    slot: Arc<(Mutex<Slot>, Condvar)>,
    results: Receiver<PhotoResult>,
    thread: Option<JoinHandle<()>>,
}

impl PhotoWorker {
    /// Start the decode/develop worker sharing the host's device. `ctx`/`pipeline` are the host's
    /// own handles; the worker wraps the pipeline in its own `Renderer` over the same
    /// device/queue. `wake` must trigger a UI redraw (the winit event-loop proxy).
    pub fn start(
        ctx: &GpuContext,
        pipeline: Arc<DevelopPipeline>,
        device_epoch: DeviceEpoch,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
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
        let runner = move |request: PhotoRequest, cache: &mut PreparedCache| {
            develop(&worker_ctx, &pipeline, &renderer, request, cache)
        };
        Self::with_runner(Box::new(runner), wake)
    }

    fn with_runner(mut runner: Box<Runner>, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        let slot = Arc::new((
            Mutex::new(Slot {
                request: None,
                stop: false,
            }),
            Condvar::new(),
        ));
        let (tx, results) = mpsc::channel();
        let thread = {
            let slot = Arc::clone(&slot);
            std::thread::Builder::new()
                .name("photo-worker".into())
                .spawn(move || worker_loop(&slot, &tx, runner.as_mut(), &*wake))
                .expect("spawn photo worker")
        };
        Self {
            slot,
            results,
            thread: Some(thread),
        }
    }

    /// Latest-wins intake: replaces an unstarted request; a request already in flight finishes.
    pub fn request(&self, request: PhotoRequest) {
        let (lock, cvar) = &*self.slot;
        let mut guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.request = Some(request);
        cvar.notify_one();
    }

    /// Non-blocking drain of finished results (UI thread, each redraw).
    pub fn drain_results(&self) -> Vec<PhotoResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            out.push(result);
        }
        out
    }

    /// Stop and join. A render in progress finishes its current GPU step; its result is dropped
    /// (the loop observes the stop flag or a dead receiver).
    pub fn shutdown(&mut self) {
        let (lock, cvar) = &*self.slot;
        lock.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stop = true;
        cvar.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for PhotoWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    slot: &(Mutex<Slot>, Condvar),
    tx: &Sender<PhotoResult>,
    runner: &mut Runner,
    wake: &(dyn Fn() + Send + Sync),
) {
    let (lock, cvar) = slot;
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
        match runner(request, &mut cache) {
            Some(result) => {
                if tx.send(result).is_err() {
                    return; // host gone; stop rather than develop into a dead channel
                }
                // The frame or its message only becomes visible on a redraw; wake the loop.
                wake();
            }
            None => return, // the runner observed shutdown; stop
        }
    }
}

/// Pool exhaustion is transient (every target of this size leased, e.g. while the presenter still
/// holds the previous frame): bounded backoff before surfacing a message.
const POOL_RETRY_LIMIT: u32 = 250;
const POOL_RETRY_SLEEP: Duration = Duration::from_millis(20);

/// Decode (or reuse the cached upload) + develop into a leased target. Worker thread only.
fn develop(
    ctx: &GpuContext,
    pipeline: &DevelopPipeline,
    renderer: &Renderer,
    request: PhotoRequest,
    cache: &mut PreparedCache,
) -> Option<PhotoResult> {
    let failed = |message: String| {
        Some(PhotoResult::Failed {
            key: request.key,
            message,
        })
    };
    if let Some(feature) = unsupported_reason(&request.params) {
        return failed(format!(
            "Darkroom can't render this photo yet: {feature} is not supported by this build — \
             nothing was changed."
        ));
    }
    // Intrinsic to the file, derived here — never trust a persisted `display_referred`
    // (legacy `decode_develop` does exactly this).
    let mut params = (*request.params).clone();
    params.display_referred = core_raw::is_display(&request.path);

    let cache_key = (
        request.key.identity.source.image_id.0,
        request.content_hash.clone(),
    );
    let prepared = match cache.get(&cache_key) {
        Some(prepared) => Arc::clone(prepared),
        None => {
            let source = match core_raw::source_from_path(&request.path) {
                Ok(source) => source,
                Err(error) => {
                    return failed(format!(
                        "could not read {}: {error}",
                        request.path.display()
                    ))
                }
            };
            let linear = match core_raw::develop_linear(&source) {
                Ok(linear) => linear,
                Err(error) => {
                    return failed(format!(
                        "could not develop {}: {error}",
                        request.path.display()
                    ))
                }
            };
            match pipeline.prepare(ctx, &linear) {
                Ok(prepared) => {
                    let prepared = Arc::new(prepared);
                    cache.put(cache_key, Arc::clone(&prepared));
                    prepared
                }
                Err(error) => return failed(format!("GPU prepare failed: {error}")),
            }
        }
    };
    for attempt in 0..POOL_RETRY_LIMIT {
        match renderer.render(request.key, &prepared, &params, &request.view) {
            Ok(lease) => return Some(PhotoResult::Frame(lease)),
            Err(render_wgpu::RenderError::PoolExhausted { .. }) => {
                std::thread::sleep(POOL_RETRY_SLEEP);
                if attempt + 1 == POOL_RETRY_LIMIT {
                    return failed("GPU render targets are busy; try again".into());
                }
            }
            Err(error) => return failed(format!("develop failed: {error}")),
        }
    }
    None
}

#[cfg(test)]
#[path = "photo_worker/tests.rs"]
mod tests;
