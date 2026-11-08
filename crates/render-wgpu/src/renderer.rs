//! Leased develop rendering on the host's GPU device.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use core_model::identity::{DeviceEpoch, FrameHandle, FrameKey};
use core_pipeline::backend::PreparedImage;
use core_pipeline::{DevelopParams, DevelopPipeline, GpuContext, PipelineError, ViewParams};
use wgpu::Texture;

use crate::pool::{FrameLease, PoolLimits, TexturePool};

/// Errors from leased rendering.
#[derive(Debug)]
pub enum RenderError {
    /// The pipeline rejected the render (bad target, invalid input, ...).
    Pipeline(PipelineError),
    /// Every pooled target of that size is leased (or live leases fill the byte cap); the caller
    /// retries later — bounded memory over unbounded allocation.
    PoolExhausted { size: (u32, u32) },
    /// One target of this size alone exceeds the pool's byte cap: waiting can never help, so the
    /// caller fails instead of retrying.
    TargetTooLarge { size: (u32, u32) },
    /// The frame key was stamped with a device epoch other than this renderer's: the caller is a
    /// worker still holding a renderer from a retired (or not yet live) device. Refused before a
    /// target is leased, so such a frame can never reach the [`crate::Presenter`].
    DeviceEpochMismatch {
        /// The epoch stamped on the requested frame key.
        key: DeviceEpoch,
        /// This renderer's epoch — the host's current device.
        renderer: DeviceEpoch,
    },
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pipeline(error) => write!(f, "pipeline: {error}"),
            Self::PoolExhausted { size } => {
                write!(f, "no free render target of size {size:?} (all leased)")
            }
            Self::TargetTooLarge { size } => {
                write!(
                    f,
                    "a render target of size {size:?} exceeds the frame pool budget"
                )
            }
            Self::DeviceEpochMismatch { key, renderer } => write!(
                f,
                "frame key is stamped device epoch {key:?} but this renderer holds {renderer:?}"
            ),
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pipeline(error) => Some(error),
            Self::PoolExhausted { .. }
            | Self::TargetTooLarge { .. }
            | Self::DeviceEpochMismatch { .. } => None,
        }
    }
}

impl From<PipelineError> for RenderError {
    fn from(error: PipelineError) -> Self {
        Self::Pipeline(error)
    }
}

/// Nominal GPU byte accounting of the frame-target pool (leased + free), for the host to sum
/// with per-image `PreparedImage::resident_bytes`. Allocation arithmetic, not measured memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceReport {
    /// Bytes of every texture the pool retains, whether leased out or free.
    pub pool_bytes: u64,
    /// The pool's hard cap (`PoolLimits::total_bytes`).
    pub pool_limit_bytes: u64,
}

/// Develops frames into leased pooled targets on the host's device.
///
/// `ctx` is a clone of the host's `GpuContext` (same `wgpu::Device`/`Queue`): the host presents
/// pooled textures on the surface, this renders into them; one device, no cross-device copies.
pub struct Renderer {
    ctx: GpuContext,
    pipeline: Arc<DevelopPipeline>,
    pool: Arc<Mutex<TexturePool>>,
    device_epoch: DeviceEpoch,
    next_handle: AtomicU64,
}

impl Renderer {
    /// Share the host's device and queue. `GpuContext` is not `Clone`, so rebuild it from the
    /// same handles to prove the renderer holds the host's device.
    pub fn new(
        ctx: GpuContext,
        pipeline: Arc<DevelopPipeline>,
        device_epoch: DeviceEpoch,
        limits: PoolLimits,
    ) -> Self {
        let ctx = GpuContext {
            device: ctx.device.clone(),
            queue: ctx.queue.clone(),
            info: ctx.info.clone(),
            max_texture_dim: ctx.max_texture_dim,
        };
        Self {
            ctx,
            pipeline,
            pool: Arc::new(Mutex::new(TexturePool::new(limits))),
            device_epoch,
            next_handle: AtomicU64::new(1),
        }
    }

    pub fn device_epoch(&self) -> DeviceEpoch {
        self.device_epoch
    }

    /// Current pool byte usage (leased + free). Read-only; no behavior change.
    pub fn resource_report(&self) -> ResourceReport {
        let pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ResourceReport {
            pool_bytes: pool.total_bytes(),
            pool_limit_bytes: pool.limits().total_bytes,
        }
    }

    /// Call `signal` every time a leased target returns to the pool, so a caller that saw
    /// [`RenderError::PoolExhausted`] can wait for a release instead of polling. Replaces any
    /// earlier signal. Runs on whichever thread drops the lease, under the pool lock: it must be
    /// cheap and must not touch this renderer.
    pub fn set_return_signal(&self, signal: Arc<dyn Fn() + Send + Sync>) {
        self.pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_return_signal(signal);
    }

    /// The shared pool, for diagnostics/tests (live counts, byte accounting).
    pub fn pool(&self) -> &Arc<Mutex<TexturePool>> {
        &self.pool
    }

    /// Develop `prepared` with `params` under `view` into a pooled `key.viewport_px`-sized
    /// target. `Err(PoolExhausted)` when every target of that size is leased; the caller retries
    /// later. The submitted work is never waited on here — the lease is returned once the work is
    /// queued (`read_back` and the GPU's ordering make each lease keep its own pixels).
    ///
    /// A key stamped with another device's epoch is refused with
    /// [`RenderError::DeviceEpochMismatch`] before anything is leased: this renderer is the device
    /// it was constructed for, and frames from a retired device must not be renderable at all.
    pub fn render(
        &self,
        key: FrameKey,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &ViewParams,
    ) -> Result<FrameLease, RenderError> {
        if key.identity.device_epoch != self.device_epoch {
            return Err(RenderError::DeviceEpochMismatch {
                key: key.identity.device_epoch,
                renderer: self.device_epoch,
            });
        }
        let (w, h) = key.viewport_px;
        let texture = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .acquire(&self.ctx.device, w, h)?;
        // From here until the lease owns it, the pool counts the texture as live. A pipeline
        // error *or a panic* (survivable: the worker re-arms) must still give the slot back.
        let mut guard = LeaseGuard::new(Arc::clone(&self.pool), texture);
        let submission = self.pipeline.render_view_into_texture(
            &self.ctx,
            prepared,
            params,
            view,
            None,
            guard.texture(),
        )?;
        let handle = FrameHandle(self.next_handle.fetch_add(1, Ordering::Relaxed));
        let texture = guard.disarm();
        let pool = Arc::downgrade(&self.pool);
        Ok(FrameLease::new(key, handle, texture, pool, submission))
    }

    /// Explicit CPU readback of a leased target (tests, diagnostics, export). Blocking; never
    /// called by the presentation path.
    pub fn read_back(&self, lease: &FrameLease) -> Result<Vec<u8>, RenderError> {
        Ok(self
            .pipeline
            .read_back_texture(&self.ctx, lease.texture())?)
    }
}

/// Borrows a pooled target for the span of a render, returning it if no [`FrameLease`] appears.
///
/// Between [`TexturePool::acquire`] and handing the texture to a lease, the pool counts it as
/// live. Any early exit in that window — a pipeline error, or a panic inside
/// `render_view_into_texture` (survivable: the develop worker re-arms, see
/// `core_pipeline::backend`) — used to free the texture while leaving its size's `live` count
/// incremented, permanently shrinking that bucket until it could never lease again. Dropping the
/// guard gives the slot back; [`Self::disarm`] transfers ownership to the lease, whose own `Drop`
/// then owns the return.
struct LeaseGuard {
    pool: Arc<Mutex<TexturePool>>,
    texture: Option<Arc<Texture>>,
}

impl LeaseGuard {
    fn new(pool: Arc<Mutex<TexturePool>>, texture: Arc<Texture>) -> Self {
        Self {
            pool,
            texture: Some(texture),
        }
    }

    /// The borrowed target, for the pipeline to render into.
    fn texture(&self) -> &Arc<Texture> {
        self.texture
            .as_ref()
            .expect("lease guard holds its texture until disarmed")
    }

    /// Take the target out of the guard: a `FrameLease` now owns it, so the guard must not
    /// return it on drop.
    fn disarm(&mut self) -> Arc<Texture> {
        self.texture
            .take()
            .expect("lease guard is disarmed exactly once")
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let Some(texture) = self.texture.take() else {
            return;
        };
        // Recovered poison, never `unwrap`: this most often runs *during* a panic, and the pool's
        // accounting is still the truth worth updating.
        let mut pool = self
            .pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pool.return_texture(&texture);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same adapter policy as `tests/support::gpu`: headless runs may skip, CI's pixel gate
    /// (`DARKROOM_REQUIRE_FIXTURES=1`) requires a real device.
    fn gpu() -> Option<GpuContext> {
        match GpuContext::new() {
            Ok(ctx) => Some(ctx),
            Err(PipelineError::NoAdapter)
                if std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none()
                    && std::env::var_os("DARKROOM_REQUIRE_GPU").is_none() =>
            {
                eprintln!("no GPU adapter — skipping");
                None
            }
            Err(error) => panic!("GPU initialization failed: {error}"),
        }
    }

    fn leased(size: (u32, u32)) -> Option<(GpuContext, Arc<Mutex<TexturePool>>, Arc<Texture>)> {
        let ctx = gpu()?;
        let pool = Arc::new(Mutex::new(TexturePool::new(PoolLimits {
            per_size: 2,
            total_bytes: 16 * 1024 * 1024,
        })));
        let texture = pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .acquire(&ctx.device, size.0, size.1)
            .expect("the first texture of a size is always creatable");
        Some((ctx, pool, texture))
    }

    // The guard models the panic case directly: unwinding runs `Drop`, which must hand the
    // `live` slot back (a leaked count is permanent pool damage).
    #[test]
    fn guard_returns_the_texture_when_the_scope_panics() {
        let Some((_ctx, pool, texture)) = leased((16, 8)) else {
            return;
        };
        let size = (16, 8);
        assert_eq!(pool.lock().unwrap().live_count(size), 1);

        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = LeaseGuard::new(Arc::clone(&pool), texture);
            panic!("simulated panic inside render_view_into_texture");
        }));
        std::panic::set_hook(hook);

        assert!(panicked.is_err(), "the simulated panic must propagate");
        let pool = pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            pool.live_count(size),
            0,
            "unwinding out of a render must release the live slot"
        );
        assert_eq!(
            pool.free_count(size),
            1,
            "the texture must be back on its size's free list"
        );
    }

    // Once the lease owns the target, the guard must stay out of its way: dropping a disarmed
    // guard returns nothing (that would hand a rendered-into texture to another render).
    #[test]
    fn disarmed_guard_does_not_return_the_texture() {
        let Some((_ctx, pool, texture)) = leased((16, 8)) else {
            return;
        };
        let size = (16, 8);
        let acquired = Arc::as_ptr(&texture);
        let mut guard = LeaseGuard::new(Arc::clone(&pool), texture);
        assert_eq!(
            Arc::as_ptr(guard.texture()),
            acquired,
            "the guard renders with the texture it was given"
        );
        let owned = guard.disarm();
        assert_eq!(Arc::as_ptr(&owned), acquired);
        drop(guard);

        let pool = pool
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            pool.live_count(size),
            1,
            "the lease owns the target now; the guard must not release it"
        );
        assert_eq!(pool.free_count(size), 0);
    }
}
