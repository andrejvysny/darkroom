//! Bounded pool of leased GPU render targets.
//!
//! One free list per `(width, height)`; the pool creates at most `PoolLimits::per_size` textures
//! per size (concurrently leased count bounded by it, and the free list only ever holds the
//! complement), and every texture the pool *still retains* — leased out or parked on a free list —
//! counts against `PoolLimits::total_bytes`. A lease's texture is never handed to another render
//! while the lease is alive; dropping the lease returns it to its size's free list.
//!
//! Free textures are a reuse cache, not a reservation: when a new size would cross the byte cap,
//! the least-recently-returned textures of *other* sizes are dropped and their bytes given back
//! before the request is refused. So `PoolExhausted` means "every target of that size is leased
//! out" (or live leases alone fill the cap) — never "this pool once saw many geometries", which
//! is what a window resize or a ptP drag would otherwise permanently cost it.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, Weak};

use wgpu::{Device, Texture, TextureFormat, TextureUsages};

use crate::renderer::RenderError;

/// Usage every pooled develop target carries: rendered into by `render_view_into_texture`,
/// copyable by the explicit `read_back` path, and sampleable as a texture — the host registers
/// the presented frame with egui, which binds it in a fragment shader (`TEXTURE_BINDING` is
/// required for the view to be bindable at all; without it egui's `egui_user_image_*` bind
/// group fails validation the moment a frame is on screen).
pub const FRAME_USAGE: TextureUsages = TextureUsages::RENDER_ATTACHMENT
    .union(TextureUsages::COPY_SRC)
    .union(TextureUsages::TEXTURE_BINDING);

/// Byte size of one `Rgba8Unorm` target.
fn target_bytes(width: u32, height: u32) -> u64 {
    width as u64 * height as u64 * 4
}

/// Memory bounds for [`TexturePool`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PoolLimits {
    /// Max concurrently-leased textures the pool keeps per `(w, h)` size.
    pub per_size: usize,
    /// Hard cap on the GPU bytes the pool *retains*: textures leased out plus textures still
    /// parked on a free list. Idle free textures are evicted (least recently returned first) to
    /// honour it; leased ones are not, so live leases always bound memory.
    pub total_bytes: u64,
}

impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            per_size: 3,
            total_bytes: 512 * 1024 * 1024,
        }
    }
}

/// A texture parked on a size's free list, stamped with the pool-wide counter value at the
/// moment it was returned. The lowest stamp has been idle longest, so it is evicted first.
#[derive(Debug)]
struct FreeTexture {
    returned_at: u64,
    texture: Arc<Texture>,
}

/// Per-size free lists plus the live/byte accounting that bounds the pool.
#[derive(Debug)]
pub struct TexturePool {
    limits: PoolLimits,
    /// `(w, h)` -> free textures, ready to be leased again. Never an empty bucket. The *back* is
    /// the most recently returned texture (handed out first, the hot one); the *front* is the
    /// eviction candidate.
    free: HashMap<(u32, u32), VecDeque<FreeTexture>>,
    /// `(w, h)` -> textures currently leased out.
    live: HashMap<(u32, u32), usize>,
    /// Bytes held by every texture the pool retains, leased *and* free. Returned textures keep
    /// their bytes (they can be leased again without allocating); eviction gives them back.
    total_bytes: u64,
    /// Monotonic stamp handed to each texture as it lands on a free list.
    next_free_stamp: u64,
}

impl TexturePool {
    pub fn new(limits: PoolLimits) -> Self {
        Self {
            limits: PoolLimits {
                per_size: limits.per_size.max(1),
                total_bytes: limits.total_bytes,
            },
            free: HashMap::new(),
            live: HashMap::new(),
            total_bytes: 0,
            next_free_stamp: 0,
        }
    }

    pub fn limits(&self) -> PoolLimits {
        self.limits
    }

    /// Total GPU bytes held by every texture the pool retains (leased + free).
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Textures currently leased out for `size` — bounded by `PoolLimits::per_size`.
    pub fn live_count(&self, size: (u32, u32)) -> usize {
        self.live.get(&size).copied().unwrap_or(0)
    }

    /// Freed textures of `size` waiting to be leased again (until the byte cap evicts them).
    pub fn free_count(&self, size: (u32, u32)) -> usize {
        self.free.get(&size).map_or(0, VecDeque::len)
    }

    /// Take a target for `size`, reusing a freed texture when one exists, else creating one if
    /// the per-size and total-byte bounds allow. Before the byte cap refuses a creation, idle
    /// textures of other sizes are evicted ([`Self::make_room_for`]), so a pool whose leases are
    /// all dropped can always serve *some* size again. `Err(PoolExhausted)` when every target of
    /// that size is leased out, or when the byte cap is filled by leases that cannot be evicted.
    pub(crate) fn acquire(
        &mut self,
        device: &Device,
        width: u32,
        height: u32,
    ) -> Result<Arc<Texture>, RenderError> {
        let size = (width.max(1), height.max(1));
        let reused = match self.free.entry(size) {
            Entry::Occupied(mut bucket) => {
                let taken = bucket.get_mut().pop_back().map(|free| free.texture);
                if bucket.get().is_empty() {
                    bucket.remove_entry();
                }
                taken
            }
            Entry::Vacant(_) => None,
        };
        if let Some(texture) = reused {
            *self.live.entry(size).or_insert(0) += 1;
            return Ok(texture);
        }
        if self.live_count(size) >= self.limits.per_size {
            // Every target of this size is being rendered into; no eviction can help that.
            return Err(RenderError::PoolExhausted { size });
        }
        let bytes = target_bytes(size.0, size.1);
        if !self.make_room_for(bytes) {
            return Err(RenderError::PoolExhausted { size });
        }
        let texture = Arc::new(device.create_texture(&wgpu::TextureDescriptor {
            label: Some("render-wgpu-frame"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TextureFormat::Rgba8Unorm,
            usage: FRAME_USAGE,
            view_formats: &[],
        }));
        *self.live.entry(size).or_insert(0) += 1;
        self.total_bytes += bytes;
        Ok(texture)
    }

    /// Make a new `bytes`-sized texture fit under [`PoolLimits::total_bytes`] by dropping idle
    /// textures, returning whether it now fits.
    ///
    /// Only *free* textures can go — a leased one is being rendered into. The candidates are the
    /// other sizes' free lists (`acquire` reuses this size's only texture if it had one, so its
    /// bucket is empty here: its next lease is never thrown away to make room for itself), dropped
    /// least-recently-returned first, so the geometry the UI touched most recently survives a
    /// resize storm. A returned texture's bytes stay counted until this point; an evicted texture
    /// is the pool's last reference, so dropping it releases the GPU memory.
    fn make_room_for(&mut self, bytes: u64) -> bool {
        if self.total_bytes.saturating_add(bytes) <= self.limits.total_bytes {
            return true;
        }
        let mut idle: Vec<(u64, (u32, u32))> = self
            .free
            .iter()
            .flat_map(|(size, bucket)| bucket.iter().map(|free| (free.returned_at, *size)))
            .collect();
        idle.sort_by_key(|(returned_at, _)| *returned_at);
        for (_, size) in idle {
            if let Some(evicted) = self.free.get_mut(&size).and_then(VecDeque::pop_front) {
                self.total_bytes = self
                    .total_bytes
                    .saturating_sub(target_bytes(size.0, size.1));
                drop(evicted);
            }
            if self.total_bytes.saturating_add(bytes) <= self.limits.total_bytes {
                break;
            }
        }
        self.free.retain(|_, bucket| !bucket.is_empty());
        self.total_bytes.saturating_add(bytes) <= self.limits.total_bytes
    }

    /// Return a dropped lease's texture to its size's free list. The texture's bytes stay counted
    /// against the cap (the pool can hand it out again without allocating); they are given back
    /// only when [`Self::make_room_for`] evicts it.
    pub(crate) fn return_texture(&mut self, texture: &Arc<Texture>) {
        let size = (texture.width(), texture.height());
        if let Some(count) = self.live.get_mut(&size) {
            *count = count.saturating_sub(1);
        }
        let returned_at = self.next_free_stamp;
        self.next_free_stamp += 1;
        self.free.entry(size).or_default().push_back(FreeTexture {
            returned_at,
            texture: Arc::clone(texture),
        });
    }
}

/// A rendered target. While alive, the pool never hands its texture to another render; on drop
/// the texture returns to the pool's free list.
#[derive(Debug)]
pub struct FrameLease {
    key: crate::FrameKey,
    handle: crate::FrameHandle,
    texture: Arc<Texture>,
    pool: Weak<Mutex<TexturePool>>,
    /// Kept so the host can `poll(device, Some(&submission))` for "frame done".
    submission: wgpu::SubmissionIndex,
}

impl FrameLease {
    pub(crate) fn new(
        key: crate::FrameKey,
        handle: crate::FrameHandle,
        texture: Arc<Texture>,
        pool: Weak<Mutex<TexturePool>>,
        submission: wgpu::SubmissionIndex,
    ) -> Self {
        Self {
            key,
            handle,
            texture,
            pool,
            submission,
        }
    }

    /// Everything this frame depends on; the [`crate::Presenter`] validates it against what the
    /// UI wants before publishing.
    pub fn key(&self) -> &crate::FrameKey {
        &self.key
    }

    /// The developed `Rgba8Unorm` target (host registers it with egui without any copy).
    pub fn texture(&self) -> &Texture {
        &self.texture
    }

    /// Opaque identity of this frame for the application layer.
    pub fn handle(&self) -> crate::FrameHandle {
        self.handle
    }

    /// The submission this render was queued under.
    pub fn submission(&self) -> &wgpu::SubmissionIndex {
        &self.submission
    }

    /// Pointer identity of the underlying texture. Two leases with the same pointer are the same
    /// texture — used by pool tests to prove reuse; application code must not rely on it.
    pub fn texture_ptr(&self) -> *const Texture {
        Arc::as_ptr(&self.texture)
    }
}

impl Drop for FrameLease {
    fn drop(&mut self) {
        // A dead or poisoned pool just drops the texture with the lease — bounded memory, no panic.
        if let Some(pool) = self.pool.upgrade() {
            if let Ok(mut pool) = pool.lock() {
                pool.return_texture(&self.texture);
            }
        }
    }
}
