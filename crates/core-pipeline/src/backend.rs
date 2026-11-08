//! wgpu develop backend (Metal on macOS, DX12 preferred on Windows).
//!
//! `prepare()` uploads the cached linear buffer to a GPU texture and allocates the per-image
//! output/readback/uniform resources ONCE. `render()` then only rewrites the uniform and redraws,
//! so slider changes re-render in single-digit milliseconds. Output resolution == input resolution.

use crate::error::PipelineError;
use crate::params::{AiCoverage, DevelopParams};
use crate::params::{GpuParamsExt, GpuViewParamsExt};
use core_raw::LinearImage;
use wgpu::util::DeviceExt;

#[cfg(target_os = "windows")]
const NVIDIA_VENDOR_ID: u32 = 0x10DE;

/// Coverage availability and pixels are part of the baked layer, even when the persisted mask
/// geometry/hash is unchanged (for example when SAM finishes rebuilding after a reload).
fn mask_render_hash(mask: &crate::params::Mask, coverages: &[AiCoverage]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    crate::mask::mask_geometry_hash(mask).hash(&mut hash);
    if let Some(key) = crate::mask::mask_ai_hash(mask) {
        let coverage = coverages.iter().find(|c| c.hash == key);
        coverage.is_some().hash(&mut hash);
        if let Some(coverage) = coverage {
            coverage.width.hash(&mut hash);
            coverage.height.hash(&mut hash);
            coverage.alpha.hash(&mut hash);
        }
    }
    hash.finish()
}

/// Long-lived GPU device + queue.
pub struct GpuContext {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: GpuAdapterInfo,
    /// `max_texture_dimension_2d` granted by the device. Bounds the largest input image and the
    /// largest viewport/export target this GPU can render (DX12 / Apple Silicon report 16384).
    pub max_texture_dim: u32,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuAdapterInfo {
    pub name: String,
    pub vendor: u32,
    pub device: u32,
    pub device_type: String,
    pub backend: String,
    pub driver: String,
    pub driver_info: String,
    pub device_pci_bus_id: String,
    pub subgroup_min_size: u32,
    pub subgroup_max_size: u32,
    pub transient_saves_memory: bool,
    /// Filled in from the granted device limits after creation (0 from the adapter-only `From`).
    pub max_texture_dim: u32,
}

impl From<wgpu::AdapterInfo> for GpuAdapterInfo {
    fn from(info: wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name,
            vendor: info.vendor,
            device: info.device,
            device_type: format!("{:?}", info.device_type),
            backend: format!("{:?}", info.backend),
            driver: info.driver,
            driver_info: info.driver_info,
            device_pci_bus_id: info.device_pci_bus_id,
            subgroup_min_size: info.subgroup_min_size,
            subgroup_max_size: info.subgroup_max_size,
            // Unknown backend support must not advertise a memory saving.
            transient_saves_memory: info.transient_saves_memory.unwrap_or(false),
            // Not available from AdapterInfo; set from the granted device limits in `GpuContext::new`.
            max_texture_dim: 0,
        }
    }
}

impl GpuContext {
    /// Share the host's surface-compatible device and queue with image processing.
    pub fn from_device_queue(
        device: wgpu::Device,
        queue: wgpu::Queue,
        adapter_info: wgpu::AdapterInfo,
    ) -> Self {
        let max_texture_dim = device.limits().max_texture_dimension_2d;
        let mut info = GpuAdapterInfo::from(adapter_info);
        info.max_texture_dim = max_texture_dim;
        Self {
            device,
            queue,
            info,
            max_texture_dim,
        }
    }

    pub fn new() -> Result<Self, PipelineError> {
        let desc = instance_descriptor();
        let enabled_backends = desc.backends;
        let instance = wgpu::Instance::new(desc);

        // Candidates are returned in preference order. We try `request_device` down the list so a
        // broken primary driver (e.g. a flaky NVIDIA install on Windows) falls back to the next
        // adapter instead of disabling GPU develop entirely.
        let candidates = candidate_adapters(&instance, enabled_backends);
        if candidates.is_empty() {
            return Err(PipelineError::NoAdapter);
        }

        let mut last_err: Option<String> = None;
        for adapter in candidates {
            let raw_info = adapter.get_info();
            // Request exactly what the adapter supports (not `Limits::default()`'s 8192 floor): this
            // never fails on a below-default adapter and unlocks the true max texture dim on capable
            // GPUs. `MemoryHints::MemoryUsage` is friendlier to shared-memory iGPUs (DX12/Vulkan
            // honor it; Metal ignores it).
            match pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("darkroom-device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                ..Default::default()
            })) {
                Ok((device, queue)) => {
                    let max_texture_dim = device.limits().max_texture_dimension_2d;
                    let mut info = GpuAdapterInfo::from(raw_info.clone());
                    info.max_texture_dim = max_texture_dim;
                    tracing::info!(
                        name = %info.name,
                        vendor = format_args!("0x{:04x}", info.vendor),
                        device = format_args!("0x{:04x}", info.device),
                        device_type = %info.device_type,
                        backend = %info.backend,
                        driver = %info.driver,
                        driver_info = %info.driver_info,
                        max_texture_dim,
                        "selected GPU adapter"
                    );
                    return Ok(Self {
                        device,
                        queue,
                        info,
                        max_texture_dim,
                    });
                }
                Err(e) => {
                    tracing::warn!(
                        name = %raw_info.name,
                        backend = ?raw_info.backend,
                        error = %e,
                        "request_device failed; trying next adapter"
                    );
                    last_err = Some(e.to_string());
                }
            }
        }
        Err(PipelineError::Device(last_err.unwrap_or_else(|| {
            "no enumerated adapter could create a device".to_string()
        })))
    }
}

fn instance_descriptor() -> wgpu::InstanceDescriptor {
    #[cfg(target_os = "windows")]
    {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        if std::env::var_os("WGPU_BACKEND").is_none() {
            // DX12 first (preferred), Vulkan as a fallback for stripped Windows SKUs / Wine where
            // DX12 enumeration is empty. The candidate sort keeps DX12 ahead of Vulkan per GPU.
            desc.backends = wgpu::Backends::DX12 | wgpu::Backends::VULKAN;
        }
        desc
    }
    #[cfg(not(target_os = "windows"))]
    {
        wgpu::InstanceDescriptor::new_without_display_handle_from_env()
    }
}

/// Adapters to try, in preference order. The caller attempts `request_device` down the list.
fn candidate_adapters(
    instance: &wgpu::Instance,
    enabled_backends: wgpu::Backends,
) -> Vec<wgpu::Adapter> {
    #[cfg(target_os = "windows")]
    {
        let mut adapters = pollster::block_on(instance.enumerate_adapters(enabled_backends));
        for adapter in &adapters {
            let info = adapter.get_info();
            tracing::debug!(
                name = %info.name,
                vendor = format_args!("0x{:04x}", info.vendor),
                device = format_args!("0x{:04x}", info.device),
                device_type = ?info.device_type,
                backend = ?info.backend,
                driver = %info.driver,
                "available GPU adapter"
            );
        }
        // Stable sort by (device tier, backend rank): NVIDIA discrete first, then any discrete, then
        // integrated, then anything else; within a tier prefer DX12 over the Vulkan fallback.
        adapters.sort_by_cached_key(|a| {
            let info = a.get_info();
            let tier: u8 = if info.vendor == NVIDIA_VENDOR_ID
                && info.device_type == wgpu::DeviceType::DiscreteGpu
            {
                0
            } else if info.device_type == wgpu::DeviceType::DiscreteGpu {
                1
            } else if info.device_type == wgpu::DeviceType::IntegratedGpu {
                2
            } else {
                3
            };
            let backend_rank: u8 = match info.backend {
                wgpu::Backend::Dx12 => 0,
                wgpu::Backend::Vulkan => 1,
                _ => 2,
            };
            (tier, backend_rank)
        });
        adapters
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = enabled_backends;
        // macOS (single Metal GPU) / Linux: let wgpu pick the high-performance adapter.
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .ok()
        .into_iter()
        .collect()
    }
}

/// Upload a 256x1 RGBA8 tone-curve LUT (`LUT_SIZE*4` bytes) to `tex`.
fn write_lut(ctx: &GpuContext, tex: &wgpu::Texture, lut: &[u8]) {
    let w = crate::curve::LUT_SIZE as u32;
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        lut,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: w,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// Upload the base tone-operator LUT (`BASE_LUT_SIZE` f32 entries) to `tex` (R32Float, Nx1).
fn write_base_lut(ctx: &GpuContext, tex: &wgpu::Texture, lut: &[f32]) {
    let w = crate::params::BASE_LUT_SIZE as u32;
    ctx.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(lut),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: w,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
}

/// Per-image GPU resources, reused across many `render()` calls (one per slider change).
/// Edge of the square AI-alpha texture (`ai_tex`). MUST match `core_analyze::sam::SAM_EDGE` — the
/// SAM working resolution, so a baked alpha (longest side = SAM_EDGE) fits without downscaling.
pub const AI_TEX_EDGE: u32 = 1024;

pub struct PreparedImage {
    pub width: u32,
    pub height: u32,
    bpr: u32,
    uniform: wgpu::Buffer,
    fx_uniform: wgpu::Buffer,
    // Global white-balance CAT matrix, rewritten per render() from the current params.
    wb_uniform: wgpu::Buffer,
    // Detail (sharpen/NR) + vignette scalars + texel size, rewritten per render().
    extra_uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    output: wgpu::Texture,
    // CPU-readback staging for `output` (padded `bpr * height`). Created on the first `render()` /
    // identity `render_view()`; GPU-resident paths (`render_view_into_texture`) never create it.
    readback_size: u64,
    readback: std::sync::OnceLock<wgpu::Buffer>,
    // Tone-curve LUT, rewritten per render() from the current params.
    lut: wgpu::Texture,
    // Scene-referred base tone operator: log-exposure LUT + its uniform, rewritten per render().
    base_lut: wgpu::Texture,
    tone_op_uniform: wgpu::Buffer,
    // Crop + straighten geometry uniform, rewritten per render().
    geom_uniform: wgpu::Buffer,
    // Viewport + mask-overlay uniform (@binding(13)), rewritten per render_view().
    view_uniform: wgpu::Buffer,
    // Color-balance-RGB grading uniform (@binding(14)), rewritten per render().
    cbrgb_uniform: wgpu::Buffer,
    // Channel-mixer uniform (@binding(15)), rewritten per render().
    chanmix_uniform: wgpu::Buffer,
    // Per-mask scalar deltas, rewritten per render() from the current params.
    mask_buffer: wgpu::Buffer,
    // Pre-pass uniform (one mask's components), rewritten per mask per render().
    prepass_uniform: wgpu::Buffer,
    // Baked AI (SAM) alpha for the current mask being pre-passed (fixed AI_TEX_EDGE² R8Unorm, valid
    // region top-left). Content rewritten per-mask in the bake loop from `ai_coverages`; the view is
    // bound once in `prepass_bind`.
    ai_tex: wgpu::Texture,
    /// Baked AI-mask coverages for THIS image, keyed by the `ComponentKind::Ai` component `hash`.
    /// Set by `develop_render` (resolved from the backend SAM cache) before a render; read in the
    /// mask bake loop. Public so the IPC layer can stash freshly computed coverage on the prepared
    /// image without a re-decode. Mutex keeps `PreparedImage: Sync` for the shared develop caches.
    pub ai_coverages: std::sync::Mutex<Vec<crate::params::AiCoverage>>,
    // Brush bake size→uv-radius uniform/bind (tiny; the image-sized brush coverage texture lives in
    // the lazy `mask_res`).
    // Kept alive for the bake bind group.
    _bake_uniform: wgpu::Buffer,
    bake_bind: wgpu::BindGroup,
    refine_uniform: wgpu::Buffer,
    // Kept alive for the bind group's texture views.
    _input: wgpu::Texture,
    // 1x1x1 R16Float array bound at @binding(5) in `bind_group` until masks are first needed.
    // Never sampled with real masks: the shader reads `mask_tex` only inside `for i < M.count`
    // (count = enabled masks) and when `VIEW.flags.y >= 0` (mask overlay); `ensure_mask_resources`
    // swaps in the full-size bind group before any render with either condition.
    placeholder_mask: wgpu::Texture,
    // Full-resolution mask array + scratch + the bind groups referencing them. Allocated on the
    // first render that needs masks and kept until the image drops (no shrink). Renders are
    // serialized by `render_lock`, so creation never races; `OnceLock` keeps `PreparedImage: Sync`.
    mask_res: std::sync::OnceLock<MaskResources>,
    view_target: std::sync::Mutex<Option<ViewTarget>>,
    // Geometry hash of the mask baked into each `mask_tex` layer (None = stale). Lets `render_to`
    // skip the pre-pass when a mask's coverage is unchanged. `Mutex` (not `RefCell`) keeps
    // `PreparedImage: Sync` so it can live in the shared develop caches.
    mask_layer_hash: std::sync::Mutex<Vec<Option<u64>>>,
    // Serializes renders on THIS image: all uniforms/LUTs/targets above are shared, so two
    // concurrent `render_to` calls would race on their contents. Held for the whole
    // render+readback; callers may share `Arc<PreparedImage>` across threads freely.
    render_lock: std::sync::Mutex<()>,
    // Hash of the last-written payload per uniform/LUT (see `WriteSlot`): `render_to` skips the
    // queue write when unchanged, so a pan/zoom frame rewrites ONLY the view uniform and a single
    // slider move rewrites only the buffers it actually feeds. Guarded by `render_lock`.
    write_hashes: std::sync::Mutex<[Option<u64>; WRITE_SLOTS]>,
}

/// Borrowed resources that make up the develop bind group (everything but the mask array).
struct DevelopBindSources<'a> {
    input_view: &'a wgpu::TextureView,
    lut_view: &'a wgpu::TextureView,
    base_lut_view: &'a wgpu::TextureView,
    uniform: &'a wgpu::Buffer,
    fx_uniform: &'a wgpu::Buffer,
    mask_buffer: &'a wgpu::Buffer,
    wb_uniform: &'a wgpu::Buffer,
    extra_uniform: &'a wgpu::Buffer,
    tone_op_uniform: &'a wgpu::Buffer,
    geom_uniform: &'a wgpu::Buffer,
    view_uniform: &'a wgpu::Buffer,
    cbrgb_uniform: &'a wgpu::Buffer,
    chanmix_uniform: &'a wgpu::Buffer,
}

/// Lazily allocated full-resolution mask state of one [`PreparedImage`].
struct MaskResources {
    // Mask alpha layers (R16Float, MASK_CAP layers). The pre-pass writes per-mask coverage here;
    // the develop pass samples it.
    mask_tex: wgpu::Texture,
    // Brush bake: per-mask scratch coverage (R16Float), sampled by the pre-pass.
    brush_tex: wgpu::Texture,
    // Mask alpha scratch: pre-pass writes here, then refine (or passthrough) writes the layer.
    // Two single-layer R16Float targets to ping-pong the separable bilateral.
    scratch_a: wgpu::Texture,
    scratch_b: wgpu::Texture,
    prepass_bind: wgpu::BindGroup,
    // Develop bind group with `mask_tex` bound at @binding(5) instead of the placeholder.
    bind_group: wgpu::BindGroup,
}

/// Nominal GPU allocation bytes held by one [`PreparedImage`], derived from the descriptors of the
/// resources it created (format bytes-per-texel x dims x layers x mip chain; buffer sizes).
///
/// These are allocation arithmetic, NOT measured resident memory: driver padding, tiling and
/// alignment are not included. No CPU copy of the upload survives `prepare()`, so there is no CPU
/// term. Mask/scratch/readback fields are 0 until the first render that needs them allocates them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceBytes {
    /// Rgba32Float source incl. the full mip chain.
    pub input_texture: u64,
    /// Rgba8Unorm full-size output target.
    pub output: u64,
    /// Uniform/storage buffers, tone + base LUT textures, and the 1x1 placeholder mask array.
    pub uniforms_and_luts: u64,
    /// Full-resolution mask alpha array (0 until allocated).
    pub masks: u64,
    /// Brush + mask-alpha scratch textures (0 until allocated).
    pub scratch: u64,
    /// CPU-readback staging buffer for `output` (0 until the first readback render).
    pub readback: u64,
    /// Fixed AI-alpha texture.
    pub ai_tex: u64,
    /// Viewport render target + its readback buffer (0 until a non-identity `render_view`).
    pub view_target: u64,
}

impl ResourceBytes {
    pub fn total(&self) -> u64 {
        self.input_texture
            + self.output
            + self.uniforms_and_luts
            + self.masks
            + self.scratch
            + self.readback
            + self.ai_tex
            + self.view_target
    }
}

/// Nominal bytes of a texture from its descriptor: texel size x dims x layers over the mip chain.
fn texture_bytes(t: &wgpu::Texture) -> u64 {
    let texel = u64::from(t.format().block_copy_size(None).unwrap_or(0));
    let layers = u64::from(t.depth_or_array_layers());
    (0..t.mip_level_count())
        .map(|l| {
            u64::from((t.width() >> l).max(1))
                * u64::from((t.height() >> l).max(1))
                * layers
                * texel
        })
        .sum()
}

fn align4(n: u64) -> u64 {
    n.div_ceil(4) * 4
}

pub(crate) fn mip_count_for(w: u32, h: u32) -> u32 {
    (32 - w.max(h).leading_zeros()).clamp(1, 14)
}

/// Nominal bytes of an image-sized single-layer texture of `bpp` bytes per texel.
fn plane_bytes(w: u32, h: u32, layers: u64, bpp: u64) -> u64 {
    u64::from(w) * u64::from(h) * layers * bpp
}

/// Sizes of the fixed-size uniform/storage buffers a prepare creates (create_buffer_init aligns to 4).
fn fixed_buffer_bytes() -> u64 {
    use crate::params as p;
    [
        std::mem::size_of::<p::ParamsUniform>(),
        std::mem::size_of::<p::FxUniform>(),
        std::mem::size_of::<p::WbUniform>(),
        std::mem::size_of::<p::ExtraUniform>(),
        std::mem::size_of::<p::ToneOpUniform>(),
        std::mem::size_of::<p::GeomUniform>(),
        std::mem::size_of::<p::ViewUniform>(),
        std::mem::size_of::<p::CbRgbUniform>(),
        std::mem::size_of::<p::ChanMixUniform>(),
        std::mem::size_of::<p::MaskBufferUniform>(),
        std::mem::size_of::<crate::mask::PrepassUniform>(),
        std::mem::size_of::<crate::mask::RefineUniform>(),
        std::mem::size_of::<crate::mask::BakeUniform>(),
    ]
    .into_iter()
    .map(|n| align4(n as u64))
    .sum()
}

/// Tone-curve LUT (Rgba8Unorm, LUT_SIZE x 1) + base-curve LUT (R32Float, BASE_LUT_SIZE x 1) +
/// 1x1x1 R16Float placeholder mask array.
fn fixed_texture_bytes() -> u64 {
    crate::curve::LUT_SIZE as u64 * 4 + crate::params::BASE_LUT_SIZE as u64 * 4 + 2
}

/// NOMINAL bytes [`DevelopPipeline::prepare`] allocates eagerly for a `width x height` image:
/// source + mip chain, output, uniforms/LUTs, placeholder mask and the AI-alpha texture. Equals
/// `PreparedImage::resident_bytes().total()` right after `prepare`. Pure; no device. Allocation
/// arithmetic, not measured memory.
pub fn estimate_prepare_bytes(width: u32, height: u32) -> u64 {
    estimate_input_bytes(width, height)
        + plane_bytes(width, height, 1, 4)
        + fixed_buffer_bytes()
        + fixed_texture_bytes()
        + plane_bytes(AI_TEX_EDGE, AI_TEX_EDGE, 1, 1)
}

/// NOMINAL extra bytes the first masked render allocates (`MASK_CAP`-layer R16Float array, brush
/// scratch and two mask-alpha scratch planes). Does not include the CPU-readback buffer
/// ([`estimate_readback_bytes`]).
pub fn estimate_mask_bytes(width: u32, height: u32) -> u64 {
    plane_bytes(width, height, crate::params::MASK_CAP as u64, 2)
        + estimate_mask_scratch_bytes(width, height)
}

fn estimate_mask_scratch_bytes(width: u32, height: u32) -> u64 {
    plane_bytes(width, height, 3, 2)
}

/// NOMINAL bytes of the lazily created CPU-readback buffer (rows padded to the copy alignment).
pub fn estimate_readback_bytes(width: u32, height: u32) -> u64 {
    let a = u64::from(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    (u64::from(width) * 4).div_ceil(a) * a * u64::from(height)
}

fn estimate_input_bytes(width: u32, height: u32) -> u64 {
    (0..mip_count_for(width, height))
        .map(|l| plane_bytes((width >> l).max(1), (height >> l).max(1), 1, 16))
        .sum()
}

impl PreparedImage {
    /// Nominal GPU allocation bytes this image holds right now, from the actual resource
    /// descriptors (see [`ResourceBytes`]). Lazy parts (masks, scratch, readback, viewport target)
    /// are 0 until a render allocates them.
    pub fn resident_bytes(&self) -> ResourceBytes {
        let buffers = [
            &self.uniform,
            &self.fx_uniform,
            &self.wb_uniform,
            &self.extra_uniform,
            &self.tone_op_uniform,
            &self.geom_uniform,
            &self.view_uniform,
            &self.cbrgb_uniform,
            &self.chanmix_uniform,
            &self.mask_buffer,
            &self.prepass_uniform,
            &self._bake_uniform,
            &self.refine_uniform,
        ];
        let uniforms_and_luts = buffers.iter().map(|b| b.size()).sum::<u64>()
            + texture_bytes(&self.lut)
            + texture_bytes(&self.base_lut)
            + texture_bytes(&self.placeholder_mask);
        let (masks, scratch) = self.mask_res.get().map_or((0, 0), |m| {
            (
                texture_bytes(&m.mask_tex),
                texture_bytes(&m.brush_tex)
                    + texture_bytes(&m.scratch_a)
                    + texture_bytes(&m.scratch_b),
            )
        });
        let view_target = self
            .view_target
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map_or(0, |t| texture_bytes(&t.texture) + t.readback.size());
        ResourceBytes {
            input_texture: texture_bytes(&self._input),
            output: texture_bytes(&self.output),
            uniforms_and_luts,
            masks,
            scratch,
            readback: self.readback.get().map_or(0, |b| b.size()),
            ai_tex: texture_bytes(&self.ai_tex),
            view_target,
        }
    }
}

/// Indices into `PreparedImage::write_hashes`.
mod write_slot {
    pub const PARAMS: usize = 0;
    pub const FX: usize = 1;
    pub const WB: usize = 2;
    pub const EXTRA: usize = 3;
    pub const MASKS: usize = 4;
    pub const VIEW: usize = 5;
    pub const TONE_LUT: usize = 6;
    pub const TONE_OP: usize = 7;
    pub const BASE_LUT: usize = 8;
    pub const GEOM: usize = 9;
    pub const CBRGB: usize = 10;
    pub const CHANMIX: usize = 11;
}
const WRITE_SLOTS: usize = 12;

/// Area-average one RGBA f32 mip level into the next. GPU mip dimensions are floor-halved, so an
/// odd source edge must be distributed over the smaller level rather than dropping its last texel.
pub(crate) fn downsample_rgba_f32(src: &[f32], w: u32, h: u32) -> Vec<f32> {
    let (nw, nh) = ((w / 2).max(1) as usize, (h / 2).max(1) as usize);
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0f32; nw * nh * 4];
    let exact_halving = (w == 1 || w.is_multiple_of(2)) && (h == 1 || h.is_multiple_of(2));
    for y in 0..nh {
        for x in 0..nw {
            let o = (y * nw + x) * 4;
            if exact_halving {
                // Preserve the existing fast 2×2 box arithmetic for even dimensions (including
                // the duplicated sample on a 1-pixel axis). Accumulate in f64 only if a sum of
                // finite HDR values overflows f32; their average is still representable.
                let y0 = (y * 2).min(h - 1);
                let y1 = (y * 2 + 1).min(h - 1);
                let x0 = (x * 2).min(w - 1);
                let x1 = (x * 2 + 1).min(w - 1);
                for c in 0..4 {
                    let samples = [
                        src[(y0 * w + x0) * 4 + c],
                        src[(y0 * w + x1) * 4 + c],
                        src[(y1 * w + x0) * 4 + c],
                        src[(y1 * w + x1) * 4 + c],
                    ];
                    let sum = samples[0] + samples[1] + samples[2] + samples[3];
                    out[o + c] = if sum.is_finite() {
                        0.25 * sum
                    } else {
                        (0.25 * samples.into_iter().map(f64::from).sum::<f64>()) as f32
                    };
                }
            } else {
                // Use integer overlap lengths in units of 1/nw and 1/nh source pixels. Each
                // destination footprint has total weight w*h, with at most 3×3 source texels.
                // This covers [0,w]×[0,h] exactly, with no dropped border or shifted footprint.
                let (left, right) = (x * w, (x + 1) * w);
                let (top, bottom) = (y * h, (y + 1) * h);
                let mut sum = [0.0f64; 4];
                for sy in top / nh..bottom.div_ceil(nh) {
                    let wy = bottom.min((sy + 1) * nh) - top.max(sy * nh);
                    for sx in left / nw..right.div_ceil(nw) {
                        let wx = right.min((sx + 1) * nw) - left.max(sx * nw);
                        let weight = (wx * wy) as f64;
                        for (c, value) in sum.iter_mut().enumerate() {
                            *value += f64::from(src[(sy * w + sx) * 4 + c]) * weight;
                        }
                    }
                }
                for (c, value) in sum.iter().enumerate() {
                    out[o + c] = (value / (w * h) as f64) as f32;
                }
            }
        }
    }
    out
}

fn payload_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Write `bytes` to `buf` only when they differ from the slot's last-written payload.
fn write_buffer_if_changed(
    queue: &wgpu::Queue,
    buf: &wgpu::Buffer,
    bytes: &[u8],
    slot: &mut Option<u64>,
) {
    let h = payload_hash(bytes);
    if *slot == Some(h) {
        return;
    }
    *slot = Some(h);
    queue.write_buffer(buf, 0, bytes);
}

/// Block until `readback` (previously filled by a `copy_texture_to_buffer` of an `ow×oh`
/// `Rgba8Unorm` texture with `out_bpr`-padded rows) is mapped, then return tightly-packed RGBA8
/// (`ow*oh*4`). Shared tail of [`DevelopPipeline::read_back`] (per-image readback buffers) and
/// [`DevelopPipeline::read_back_texture`] (fresh staging buffer for a caller-owned texture).
fn read_back_mapped(
    ctx: &GpuContext,
    readback: &wgpu::Buffer,
    out_bpr: u32,
    ow: u32,
    oh: u32,
) -> Result<Vec<u8>, PipelineError> {
    // The source/viewport layout was validated before allocation. Reserve the host output
    // before mapping the readback so an allocation error cannot leave that GPU buffer mapped.
    let output_len = ow as usize * oh as usize * 4;
    let mut out = Vec::new();
    out.try_reserve_exact(output_len)
        .map_err(|e| PipelineError::Allocation(format!("CPU readback: {e}")))?;

    let (tx, rx) = std::sync::mpsc::channel();
    readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = ctx.device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv()
        .map_err(|e| PipelineError::Map(e.to_string()))?
        .map_err(|e| PipelineError::Map(e.to_string()))?;

    let data = match readback.slice(..).get_mapped_range() {
        Ok(data) => data,
        Err(error) => {
            readback.unmap();
            return Err(PipelineError::Map(error.to_string()));
        }
    };
    out.resize(output_len, 0u8);
    let row = ow as usize * 4;
    if out_bpr as usize == row {
        // Rows already tightly packed (width divisible by the 256-byte alignment) — one copy.
        let len = out.len();
        out.copy_from_slice(&data[..len]);
    } else {
        for y in 0..oh as usize {
            let src = y * out_bpr as usize;
            let dst = y * row;
            out[dst..dst + row].copy_from_slice(&data[src..src + row]);
        }
    }
    drop(data);
    readback.unmap();
    Ok(out)
}

/// Validate a caller-owned texture passed to [`DevelopPipeline::render_view_into_texture`]
/// against the requested view. Typed error on every mismatch — a wrong texture must never panic
/// the render thread.
fn validate_render_target(
    device: &wgpu::Device,
    target: &wgpu::Texture,
    view: &crate::params::ViewParams,
) -> Result<(), PipelineError> {
    let bad = |why: String| PipelineError::InvalidTarget(why);
    if target.format() != wgpu::TextureFormat::Rgba8Unorm {
        return Err(bad(format!(
            "target format {:?} is not Rgba8Unorm",
            target.format()
        )));
    }
    if !target
        .usage()
        .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
    {
        return Err(bad("target lacks RENDER_ATTACHMENT usage".to_string()));
    }
    if target.dimension() != wgpu::TextureDimension::D2
        || target.mip_level_count() != 1
        || target.sample_count() != 1
    {
        return Err(bad(format!(
            "target must be a single-sampled 2D texture (dimension {:?}, {} mips, {} samples)",
            target.dimension(),
            target.mip_level_count(),
            target.sample_count()
        )));
    }
    let limits = device.limits();
    if target.width() != view.out_w || target.height() != view.out_h {
        return Err(bad(format!(
            "target is {}x{}, view output is {}x{}",
            target.width(),
            target.height(),
            view.out_w,
            view.out_h
        )));
    }
    if view.out_w == 0
        || view.out_h == 0
        || view.out_w > limits.max_texture_dimension_2d
        || view.out_h > limits.max_texture_dimension_2d
    {
        return Err(bad(format!(
            "view output {}x{} outside 1..={} px",
            view.out_w, view.out_h, limits.max_texture_dimension_2d
        )));
    }
    Ok(())
}

struct ViewTarget {
    width: u32,
    height: u32,
    bpr: u32,
    texture: wgpu::Texture,
    readback: wgpu::Buffer,
}

impl ViewTarget {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Self, PipelineError> {
        let limits = device.limits();
        let (bpr, readback_size) = crate::validation::output_layout(
            width,
            height,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-view-output"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop-view-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(PipelineError::Allocation(format!("GPU viewport: {err}")));
        }
        Ok(Self {
            width,
            height,
            bpr,
            texture,
            readback,
        })
    }
}

/// The reusable develop render pipeline (created once per `GpuContext`).
pub struct DevelopPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    // Filtering sampler for the mask alpha array (linear, clamp).
    mask_sampler: wgpu::Sampler,
    // Mask pre-pass (parametric coverage → per-mask alpha layer).
    prepass: crate::mask::MaskPrepass,
    // Brush bake (strokes → per-mask brush coverage texture).
    brush_bake: crate::mask::BrushBake,
    // Edge-aware mask refinement (cross-bilateral).
    refine: crate::mask::MaskRefine,
    // GPU mip-chain builder; `None` when the compute pipeline is rejected (CPU mips then).
    mips: Option<crate::mips::MipGenerator>,
}

impl DevelopPipeline {
    pub fn new(ctx: &GpuContext) -> Self {
        let device = &ctx.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("develop-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("develop.wgsl").into()),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("develop-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Tone-curve LUT (256x1 RGBA8), read via textureLoad (no sampler needed).
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Secondary effects uniform (HSL bands; grows with crop/lens/detail later).
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Mask alpha layers (R16Float D2 array, filterable).
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
                        multisampled: false,
                    },
                    count: None,
                },
                // Filtering sampler for the mask alpha array.
                wgpu::BindGroupLayoutEntry {
                    binding: 6,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                // Per-mask scalar deltas (read-only storage buffer).
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Global white-balance CAT matrix (uniform).
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Detail (sharpen/NR) + vignette + texel size (uniform).
                wgpu::BindGroupLayoutEntry {
                    binding: 9,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Base tone operator uniform (log-exposure domain + hue-protect).
                wgpu::BindGroupLayoutEntry {
                    binding: 10,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Base tone-operator LUT (R32Float Nx1), read via textureLoad (no sampler needed).
                wgpu::BindGroupLayoutEntry {
                    binding: 11,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // Crop + straighten geometry (uniform).
                wgpu::BindGroupLayoutEntry {
                    binding: 12,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Viewport + mask-overlay (uniform).
                wgpu::BindGroupLayoutEntry {
                    binding: 13,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Color-balance-RGB grading (uniform).
                wgpu::BindGroupLayoutEntry {
                    binding: 14,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Channel mixer (uniform).
                wgpu::BindGroupLayoutEntry {
                    binding: 15,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("develop-pl"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("develop-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("develop-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        // Filtering sampler for the mask alpha array (smooth coverage interpolation).
        let mask_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("develop-mask-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });

        let prepass = crate::mask::MaskPrepass::new(device);
        let brush_bake = crate::mask::BrushBake::new(device);
        let refine = crate::mask::MaskRefine::new(device);

        Self {
            pipeline,
            bind_layout,
            sampler,
            mask_sampler,
            prepass,
            brush_bake,
            refine,
            mips: crate::mips::MipGenerator::new(device),
        }
    }

    /// GPU mip generator unless disabled by `DARKROOM_CPU_MIPS`.
    fn mip_gen(&self) -> Option<&crate::mips::MipGenerator> {
        self.mips
            .as_ref()
            .filter(|_| std::env::var_os("DARKROOM_CPU_MIPS").is_none())
    }

    /// Test hook: build the input mip chain on the GPU (`gpu`) or CPU and read back every level.
    #[doc(hidden)]
    pub fn debug_mip_levels(
        &self,
        ctx: &GpuContext,
        img: &LinearImage,
        gpu: bool,
    ) -> Result<Vec<Vec<f32>>, PipelineError> {
        let limits = ctx.device.limits();
        let rgba_len = crate::validation::linear_image(
            img,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;
        let gen = if gpu {
            Some(
                self.mips
                    .as_ref()
                    .ok_or_else(|| PipelineError::Device("GPU mip generator unavailable".into()))?,
            )
        } else {
            None
        };
        crate::mips::read_levels(ctx, gen, img, rgba_len)
    }

    /// The develop pass bind group (bindings 0-15) with `mask_view` at @binding(5).
    fn develop_bind_group(
        &self,
        device: &wgpu::Device,
        s: &DevelopBindSources<'_>,
        mask_view: &wgpu::TextureView,
    ) -> wgpu::BindGroup {
        fn buf(binding: u32, b: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: b.as_entire_binding(),
            }
        }
        fn tex(binding: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: wgpu::BindingResource::TextureView(v),
            }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop-bg"),
            layout: &self.bind_layout,
            entries: &[
                tex(0, s.input_view),
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                buf(2, s.uniform),
                tex(3, s.lut_view),
                buf(4, s.fx_uniform),
                tex(5, mask_view),
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                buf(7, s.mask_buffer),
                buf(8, s.wb_uniform),
                buf(9, s.extra_uniform),
                buf(10, s.tone_op_uniform),
                tex(11, s.base_lut_view),
                buf(12, s.geom_uniform),
                buf(13, s.view_uniform),
                buf(14, s.cbrgb_uniform),
                buf(15, s.chanmix_uniform),
            ],
        })
    }

    /// Allocate (once) the full-resolution mask array, brush/mask scratch and the bind groups that
    /// reference them. Called by `encode_develop` before any render with >=1 enabled mask or a mask
    /// overlay. Caller holds `prepared.render_lock`, so there is no creation race. Never shrinks.
    fn ensure_mask_resources<'a>(
        &self,
        ctx: &GpuContext,
        prepared: &'a PreparedImage,
    ) -> Result<&'a MaskResources, PipelineError> {
        if let Some(res) = prepared.mask_res.get() {
            return Ok(res);
        }
        let device = &ctx.device;
        let (w, h) = (prepared.width, prepared.height);
        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        // Mask alpha layers: R16Float, MASK_CAP layers, same size as the image. Zero-initialised by
        // wgpu; the pre-pass writes real coverage via RENDER_ATTACHMENT (fragment-to-texture —
        // R16Float is not storage-bindable on Metal).
        let mask_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-mask-alpha"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: crate::params::MASK_CAP as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Single-layer image-sized R16Float: brush coverage + the pre-pass/refine ping-pong pair.
        let r16_plane = |label: &str| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R16Float,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            })
        };
        let brush_tex = r16_plane("develop-brush-scratch");
        let scratch_a = r16_plane("mask-scratch-a");
        let scratch_b = r16_plane("mask-scratch-b");

        let input_view = prepared
            ._input
            .create_view(&wgpu::TextureViewDescriptor::default());
        let ai_tex_view = prepared
            .ai_tex
            .create_view(&wgpu::TextureViewDescriptor::default());
        let brush_scratch_view = brush_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let prepass_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop-prepass-bg"),
            layout: &self.prepass.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: prepared.prepass_uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&brush_scratch_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&ai_tex_view),
                },
            ],
        });
        let lut_view = prepared
            .lut
            .create_view(&wgpu::TextureViewDescriptor::default());
        let base_lut_view = prepared
            .base_lut
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = mask_tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("develop-mask-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = self.develop_bind_group(
            device,
            &DevelopBindSources {
                input_view: &input_view,
                lut_view: &lut_view,
                base_lut_view: &base_lut_view,
                uniform: &prepared.uniform,
                fx_uniform: &prepared.fx_uniform,
                mask_buffer: &prepared.mask_buffer,
                wb_uniform: &prepared.wb_uniform,
                extra_uniform: &prepared.extra_uniform,
                tone_op_uniform: &prepared.tone_op_uniform,
                geom_uniform: &prepared.geom_uniform,
                view_uniform: &prepared.view_uniform,
                cbrgb_uniform: &prepared.cbrgb_uniform,
                chanmix_uniform: &prepared.chanmix_uniform,
            },
            &mask_view,
        );
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(PipelineError::Device(format!(
                "GPU mask allocation failed: {err}"
            )));
        }
        // Serialized by render_lock: the set cannot lose a race; if it somehow did, use the winner.
        let _ = prepared.mask_res.set(MaskResources {
            mask_tex,
            brush_tex,
            scratch_a,
            scratch_b,
            prepass_bind,
            bind_group,
        });
        Ok(prepared.mask_res.get().expect("mask resources just set"))
    }

    /// The CPU-readback buffer for `prepared.output`, created on first use.
    fn readback_buffer<'a>(
        &self,
        ctx: &GpuContext,
        prepared: &'a PreparedImage,
    ) -> Result<&'a wgpu::Buffer, PipelineError> {
        if let Some(b) = prepared.readback.get() {
            return Ok(b);
        }
        let scope = ctx.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop-readback"),
            size: prepared.readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(PipelineError::Device(format!(
                "GPU readback allocation failed: {err}"
            )));
        }
        let _ = prepared.readback.set(buffer);
        Ok(prepared.readback.get().expect("readback just set"))
    }
    /// Upload `img` (linear RGB) and allocate per-image GPU resources.
    /// Returns an error (rather than panicking) if a GPU allocation fails — e.g. a full-resolution
    /// export texture that exceeds device memory.
    pub fn prepare(
        &self,
        ctx: &GpuContext,
        img: &LinearImage,
    ) -> Result<PreparedImage, PipelineError> {
        let device = &ctx.device;
        let (w, h) = (img.width, img.height);
        let limits = device.limits();
        let rgba_len = crate::validation::linear_image(
            img,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;
        let (bpr, readback_size) = crate::validation::output_layout(
            w,
            h,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;

        // RGB f32 -> RGBA f32 (alpha = 1), parallel.
        let rgba = crate::mips::rgb_to_rgba(img, rgba_len)?;

        let scope = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        let size = wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        };
        // Full mip chain (down to 1x1), built once per prepare: zoomed-OUT viewport renders sample a
        // matching lower level instead of undersampling full res (aliasing/shimmer), and the Presence
        // (clarity/texture/dehaze) local-contrast stage reads coarser levels relative to the display
        // mip. Level 0 is the exact source; the shader picks level 0 for identity/legacy renders, so
        // goldens are untouched. Levels 1.. come from a compute shader (CPU box filter fallback).
        let input = crate::mips::upload_chain(
            ctx,
            self.mip_gen(),
            rgba,
            (w, h),
            wgpu::TextureUsages::empty(),
        );

        let output = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-output"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-uniform"),
            contents: bytemuck::bytes_of(&DevelopParams::default().to_uniform()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let fx_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-fx-uniform"),
            contents: bytemuck::bytes_of(&crate::params::FxUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let wb_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-wb-uniform"),
            contents: bytemuck::bytes_of(&crate::params::WbUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let extra_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-extra-uniform"),
            contents: bytemuck::bytes_of(&crate::params::ExtraUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let tone_op_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-tone-op-uniform"),
            contents: bytemuck::bytes_of(&crate::params::ToneOpUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let geom_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-geom-uniform"),
            contents: bytemuck::bytes_of(&crate::params::GeomUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let view_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-view-uniform"),
            contents: bytemuck::bytes_of(&crate::params::ViewUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let cbrgb_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-cbrgb-uniform"),
            contents: bytemuck::bytes_of(&crate::params::CbRgbUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let chanmix_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-chanmix-uniform"),
            contents: bytemuck::bytes_of(&crate::params::ChanMixUniform::default()),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Tone-curve LUT: 256x1 RGBA8, seeded with identity (overwritten each render()).
        let lut_size = wgpu::Extent3d {
            width: crate::curve::LUT_SIZE as u32,
            height: 1,
            depth_or_array_layers: 1,
        };
        let lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-lut"),
            size: lut_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_lut(ctx, &lut, &crate::curve::identity_lut());

        // Base tone-operator LUT: R32Float Nx1 over the log-exposure domain (overwritten each
        // render()). Seeded with the default (full-amount ACR) curve.
        let base_lut = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-base-lut"),
            size: wgpu::Extent3d {
                width: crate::params::BASE_LUT_SIZE as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        write_base_lut(ctx, &base_lut, &DevelopParams::default().base_curve_lut());

        // Placeholder mask array (1x1, 1 layer) satisfying the bind layout until a render needs masks;
        // the full MASK_CAP-layer image-sized array is allocated lazily (`ensure_mask_resources`).
        let placeholder_mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-mask-placeholder"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });

        // Per-mask scalar deltas storage buffer (default = count 0 ⇒ masking is a no-op).
        let mask_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-mask-buffer"),
            contents: bytemuck::bytes_of(&crate::params::MaskBufferUniform::default()),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        });

        let input_view = input.create_view(&wgpu::TextureViewDescriptor::default());

        // Pre-pass uniform (one mask's components at a time, rewritten per mask in render()).
        let prepass_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop-prepass-uniform"),
            size: std::mem::size_of::<crate::mask::PrepassUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // AI (SAM) alpha for the current mask: fixed AI_TEX_EDGE² R8Unorm, valid region top-left. The
        // baked mask (≤ AI_TEX_EDGE longest side, same aspect as the image) is uploaded per-mask in the
        // bake loop and sampled by normalized UV (GPU-upscaled). Fixed size ⇒ the view stays valid in
        // `prepass_bind` across images/masks (only the CONTENT changes via write_texture).
        let ai_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("develop-ai-alpha"),
            size: wgpu::Extent3d {
                width: AI_TEX_EDGE,
                height: AI_TEX_EDGE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let refine_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mask-refine-uniform"),
            size: std::mem::size_of::<crate::mask::RefineUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bake_uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("develop-bake-uniform"),
            contents: bytemuck::bytes_of(&crate::mask::BakeUniform::for_size(w, h)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bake_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("develop-bake-bg"),
            layout: &self.brush_bake.bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: bake_uniform.as_entire_binding(),
            }],
        });

        let lut_view = lut.create_view(&wgpu::TextureViewDescriptor::default());
        let base_lut_view = base_lut.create_view(&wgpu::TextureViewDescriptor::default());
        let mask_view = placeholder_mask.create_view(&wgpu::TextureViewDescriptor {
            label: Some("develop-mask-placeholder-view"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let bind_group = self.develop_bind_group(
            device,
            &DevelopBindSources {
                input_view: &input_view,
                lut_view: &lut_view,
                base_lut_view: &base_lut_view,
                uniform: &uniform,
                fx_uniform: &fx_uniform,
                mask_buffer: &mask_buffer,
                wb_uniform: &wb_uniform,
                extra_uniform: &extra_uniform,
                tone_op_uniform: &tone_op_uniform,
                geom_uniform: &geom_uniform,
                view_uniform: &view_uniform,
                cbrgb_uniform: &cbrgb_uniform,
                chanmix_uniform: &chanmix_uniform,
            },
            &mask_view,
        );

        if let Some(err) = pollster::block_on(scope.pop()) {
            return Err(PipelineError::Device(format!(
                "GPU allocation failed: {err}"
            )));
        }
        Ok(PreparedImage {
            width: w,
            height: h,
            bpr,
            uniform,
            fx_uniform,
            wb_uniform,
            extra_uniform,
            bind_group,
            output,
            readback_size,
            readback: std::sync::OnceLock::new(),
            lut,
            base_lut,
            tone_op_uniform,
            geom_uniform,
            view_uniform,
            cbrgb_uniform,
            chanmix_uniform,
            mask_buffer,
            prepass_uniform,
            ai_tex,
            ai_coverages: std::sync::Mutex::new(Vec::new()),
            _bake_uniform: bake_uniform,
            bake_bind,
            refine_uniform,
            _input: input,
            placeholder_mask,
            mask_res: std::sync::OnceLock::new(),
            view_target: std::sync::Mutex::new(None),
            mask_layer_hash: std::sync::Mutex::new(vec![None; crate::params::MASK_CAP]),
            render_lock: std::sync::Mutex::new(()),
            write_hashes: std::sync::Mutex::new([None; WRITE_SLOTS]),
        })
    }

    /// Core render: write uniforms, run the mask pre-pass, draw into `out_tex`, read back
    /// tightly-packed RGBA8 (`ow*oh*4`). Source dims (`w`,`h` from `prepared`) drive sampling /
    /// detail / mask generation; output dims (`ow`,`oh`) drive the render target + readback. `view`
    /// selects the viewport window + mask overlay (identity `ViewParams::full` ⇒ legacy full frame).
    #[allow(clippy::too_many_arguments)]
    fn render_to(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &crate::params::ViewParams,
        ai_coverages: Option<&[AiCoverage]>,
        out_tex: &wgpu::Texture,
        readback: &wgpu::Buffer,
        out_bpr: u32,
        ow: u32,
        oh: u32,
    ) -> Result<Vec<u8>, PipelineError> {
        // One render at a time per image — every uniform/LUT/target below is shared state.
        // Poison policy: RECOVER. This mutex guards ordering, not data (`Mutex<()>` — there is no
        // state inside it to be left inconsistent). A panic in one GPU render must not make every
        // later render of that image panic too, which would kill the app on the second failure.
        // Held for encode + copy + submit + readback as one unit: `readback` is shared too, so the
        // two halves must never run under separate acquisitions.
        let _render_guard = prepared
            .render_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let output_view = out_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let enc = self.encode_develop(
            ctx,
            prepared,
            params,
            view,
            ai_coverages,
            &output_view,
            (ow, oh),
        )?;
        self.read_back(ctx, out_tex, readback, out_bpr, ow, oh, enc)
    }

    /// Everything of a develop frame up to and including the main render pass into `target_view`:
    /// uniform writes, LUT uploads, brush bake, mask pre-pass/refine, then the develop pass drawing
    /// into `target_view`. Does NOT submit — the caller adds its tail (a `copy_texture_to_buffer`
    /// via [`Self::read_back`], or nothing at all for the GPU-resident path) and submits. Same
    /// order, same shared-state usage as the pre-split `render_to`.
    ///
    /// Caller MUST hold `prepared.render_lock`: every uniform/LUT/mask buffer written here is
    /// per-image shared state.
    #[allow(clippy::too_many_arguments)]
    fn encode_develop(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &crate::params::ViewParams,
        ai_coverages: Option<&[AiCoverage]>,
        target_view: &wgpu::TextureView,
        target_size: (u32, u32),
    ) -> Result<wgpu::CommandEncoder, PipelineError> {
        let device = &ctx.device;
        let (w, h) = (prepared.width, prepared.height);
        let (ow, oh) = target_size;

        // Every write below is diffed against its last payload (write_hashes): a pan/zoom frame
        // rewrites ONLY the view uniform; a slider move rewrites only what it feeds.
        // Poison policy: RECOVER + RESET. These hashes are cache-validity data ("what is currently in
        // each GPU buffer"); a panic can leave them mid-update and no longer describing the real
        // buffer contents, so trusting them would skip a write that is actually needed. `None` is
        // `write_buffer_if_changed`'s never-matches value, so clearing forces a full rewrite.
        let mut hashes = prepared.write_hashes.lock().unwrap_or_else(|p| {
            let mut g = p.into_inner();
            g.fill(None);
            g
        });
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.uniform,
            bytemuck::bytes_of(&params.to_uniform()),
            &mut hashes[write_slot::PARAMS],
        );
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.fx_uniform,
            bytemuck::bytes_of(&params.to_fx()),
            &mut hashes[write_slot::FX],
        );
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.wb_uniform,
            bytemuck::bytes_of(&params.to_wb_uniform()),
            &mut hashes[write_slot::WB],
        );
        let texel = [1.0 / w.max(1) as f32, 1.0 / h.max(1) as f32];
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.extra_uniform,
            bytemuck::bytes_of(&params.to_extra(texel)),
            &mut hashes[write_slot::EXTRA],
        );
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.mask_buffer,
            bytemuck::bytes_of(&params.to_mask_buffer()),
            &mut hashes[write_slot::MASKS],
        );
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.view_uniform,
            bytemuck::bytes_of(&view.to_uniform()),
            &mut hashes[write_slot::VIEW],
        );

        // Refresh the tone-curve LUT (identity is cheap; skips spline work when no curve set).
        // The GPU upload is skipped when the LUT bytes are unchanged.
        let lut = if params.tone_curve.is_identity() {
            crate::curve::identity_lut()
        } else {
            crate::curve::build_lut(&params.tone_curve)
        };
        {
            let h = payload_hash(&lut);
            if hashes[write_slot::TONE_LUT] != Some(h) {
                hashes[write_slot::TONE_LUT] = Some(h);
                write_lut(ctx, &prepared.lut, &lut);
            }
        }

        // Refresh the scene-referred base tone operator (uniform + log-exposure LUT). Always on
        // (mandatory base render transform); cheap to rebuild (BASE_LUT_SIZE f32 entries).
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.tone_op_uniform,
            bytemuck::bytes_of(&params.to_tone_op()),
            &mut hashes[write_slot::TONE_OP],
        );
        {
            let base = params.base_curve_lut();
            let h = payload_hash(bytemuck::cast_slice(&base));
            if hashes[write_slot::BASE_LUT] != Some(h) {
                hashes[write_slot::BASE_LUT] = Some(h);
                write_base_lut(ctx, &prepared.base_lut, &base);
            }
        }

        // Crop + straighten geometry. `src_aspect` from the source; `out_aspect` from the render
        // target. For the legacy/identity path (out == source dims) these are equal, so the geom is
        // unchanged. In the active viewport path `crop_to_source` ignores `out_aspect` anyway.
        let src_aspect = w.max(1) as f32 / h.max(1) as f32;
        let out_aspect = ow.max(1) as f32 / oh.max(1) as f32;
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.geom_uniform,
            bytemuck::bytes_of(&params.to_geom(src_aspect, out_aspect)),
            &mut hashes[write_slot::GEOM],
        );
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.cbrgb_uniform,
            bytemuck::bytes_of(&params.to_cbrgb()),
            &mut hashes[write_slot::CBRGB],
        );
        write_buffer_if_changed(
            &ctx.queue,
            &prepared.chanmix_uniform,
            bytemuck::bytes_of(&params.to_chanmix()),
            &mut hashes[write_slot::CHANMIX],
        );
        drop(hashes);

        // Mask pre-pass: compute each enabled mask's composited alpha into its alpha layer. Same
        // order as `to_mask_buffer` (enabled masks, dense 0..count). One submit per mask so the
        // per-mask uniform write is ordered before its draw (the uniform buffer is reused).
        // Cache: a mask layer is recomputed only when its coverage geometry changes. Pan/zoom and
        // global/local SCALAR edits leave geometry untouched, so they reuse the persistent mask_tex
        // layer and skip the (full-res) pre-pass entirely.
        // Poison policy: RECOVER + RESET — same reasoning as `write_hashes`. These say which mask
        // geometry is baked into each `mask_tex` layer; if that record may be stale, re-run the
        // pre-pass for every layer rather than sample coverage that was never written.
        let mut layer_hashes = prepared.mask_layer_hash.lock().unwrap_or_else(|p| {
            let mut g = p.into_inner();
            g.fill(None);
            g
        });
        // Poison policy: RECOVER ONLY — read-only here, and this holds baked SAM coverage that costs
        // a full segmentation run to rebuild, so it is never cleared. Worst case a stale coverage is
        // used for one render; the next `develop_render` re-resolves it from the backend SAM cache.
        // Application callers pass an immutable snapshot with the render request. Legacy callers
        // can still use the stored coverages; never replace that shared slot before taking the
        // render lock, since another request could otherwise render with the wrong snapshot.
        let stored_coverages = ai_coverages.is_none().then(|| {
            prepared
                .ai_coverages
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        let ai_covs = ai_coverages.unwrap_or_else(|| {
            stored_coverages
                .as_ref()
                .expect("stored coverages")
                .as_slice()
        });
        crate::validation::ai_coverages(ai_covs)?;
        // Full-res mask state is allocated only when the shader would sample `mask_tex`: the
        // `for i < M.count` loops (count = enabled masks, capped) or the mask overlay
        // (`VIEW.flags.y >= 0`). Otherwise the placeholder bind group is used and the placeholder
        // is never sampled.
        let needs_masks = view.overlay_layer >= 0 || params.masks.iter().any(|m| m.enabled);
        let mres = if needs_masks {
            Some(self.ensure_mask_resources(ctx, prepared)?)
        } else {
            None
        };
        let mut enabled_count = 0usize;
        for (layer, mask) in params
            .masks
            .iter()
            .filter(|m| m.enabled)
            .take(crate::params::MASK_CAP)
            .enumerate()
        {
            enabled_count = layer + 1;
            let geom_hash = mask_render_hash(mask, ai_covs);
            if layer_hashes[layer] == Some(geom_hash) {
                continue; // coverage unchanged ⇒ this mask_tex layer is already valid
            }
            layer_hashes[layer] = Some(geom_hash);

            // Enabled masks exist here, so `needs_masks` held and `mres` is populated.
            let mres = mres.expect("mask resources allocated for enabled masks");
            // Bake this mask's brush strokes (if any) into the brush scratch before its pre-pass.
            if crate::mask::mask_has_brush(mask) {
                self.bake_brush(ctx, prepared, mres, mask);
            }

            let mut pre = crate::mask::PrepassUniform::from_mask(mask);
            // AI (SAM) coverage: upload this mask's baked alpha into `ai_tex` (top-left) and set the
            // valid-region scale so the pre-pass samples only the written region. Missing coverage
            // (not yet computed / cache miss) leaves ai_scale=[0,0] ⇒ kind==5 reads 0 (empty mask).
            if let Some(hash) = crate::mask::mask_ai_hash(mask) {
                if let Some(cov) = ai_covs
                    .iter()
                    .find(|c| c.hash == hash && !c.alpha.is_empty())
                {
                    let cw = cov.width.min(AI_TEX_EDGE);
                    let ch = cov.height.min(AI_TEX_EDGE);
                    if cov.alpha.len() >= (cw as usize) * (ch as usize) {
                        ctx.queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &prepared.ai_tex,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            &cov.alpha,
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(cov.width),
                                rows_per_image: Some(ch),
                            },
                            wgpu::Extent3d {
                                width: cw,
                                height: ch,
                                depth_or_array_layers: 1,
                            },
                        );
                        pre.ai_scale = [
                            cw as f32 / AI_TEX_EDGE as f32,
                            ch as f32 / AI_TEX_EDGE as f32,
                        ];
                    }
                }
            }
            ctx.queue
                .write_buffer(&prepared.prepass_uniform, 0, bytemuck::bytes_of(&pre));
            let layer_view = mres.mask_tex.create_view(&wgpu::TextureViewDescriptor {
                label: Some("mask-layer-view"),
                dimension: Some(wgpu::TextureViewDimension::D2),
                base_array_layer: layer as u32,
                array_layer_count: Some(1),
                ..Default::default()
            });
            // Pre-pass writes the composited alpha into scratch_a.
            let scratch_a_view = mres
                .scratch_a
                .create_view(&wgpu::TextureViewDescriptor::default());
            let mut penc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mask-prepass-enc"),
            });
            {
                let mut pass = penc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("mask-prepass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &scratch_a_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.prepass.pipeline);
                pass.set_bind_group(0, &mres.prepass_bind, &[]);
                pass.draw(0..3, 0..1);
            }
            ctx.queue.submit(Some(penc.finish()));

            // Refine into the alpha layer. Feathered: bilateral H (a→b) then V (b→layer).
            // Otherwise a single passthrough copy (a→layer).
            let l = w.max(h) as f32;
            let texel = [1.0 / w as f32, 1.0 / h as f32];
            let luma_sigma = 0.10f32;
            if crate::mask::mask_feathered(mask) {
                let sigma_px = (0.006 * l).max(1.0);
                let scratch_b_view = mres
                    .scratch_b
                    .create_view(&wgpu::TextureViewDescriptor::default());
                self.refine_pass(
                    ctx,
                    prepared,
                    &scratch_a_view,
                    &scratch_b_view,
                    [1.0, 0.0],
                    sigma_px,
                    luma_sigma,
                    texel,
                );
                self.refine_pass(
                    ctx,
                    prepared,
                    &scratch_b_view,
                    &layer_view,
                    [0.0, 1.0],
                    sigma_px,
                    luma_sigma,
                    texel,
                );
            } else {
                // Passthrough copy (sigma 0 ⇒ shader returns the source unchanged).
                self.refine_pass(
                    ctx,
                    prepared,
                    &scratch_a_view,
                    &layer_view,
                    [0.0, 0.0],
                    0.0,
                    luma_sigma,
                    texel,
                );
            }
        }
        // Invalidate layers no longer in use so a later render with more masks recomputes them
        // instead of reusing stale coverage.
        for slot in layer_hashes.iter_mut().skip(enabled_count) {
            *slot = None;
        }
        drop(layer_hashes);

        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("develop-enc"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("develop-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, mres.map_or(&prepared.bind_group, |r| &r.bind_group), &[]);
            pass.draw(0..3, 0..1);
        }
        Ok(enc)
    }

    /// Readback tail of [`Self::render_to`]: append the target→buffer copy to the encoder from
    /// [`Self::encode_develop`], submit, block until the buffer is mapped, and return
    /// tightly-packed RGBA8 (`ow*oh*4`, rows unpadded from `out_bpr`).
    ///
    /// Caller MUST hold `prepared.render_lock` — `readback` is a per-image shared buffer, so
    /// encode and this tail belong to one lock acquisition.
    #[allow(clippy::too_many_arguments)]
    fn read_back(
        &self,
        ctx: &GpuContext,
        out_tex: &wgpu::Texture,
        readback: &wgpu::Buffer,
        out_bpr: u32,
        ow: u32,
        oh: u32,
        mut enc: wgpu::CommandEncoder,
    ) -> Result<Vec<u8>, PipelineError> {
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: out_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(out_bpr),
                    rows_per_image: Some(oh),
                },
            },
            wgpu::Extent3d {
                width: ow,
                height: oh,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(enc.finish()));
        read_back_mapped(ctx, readback, out_bpr, ow, oh)
    }

    /// Render `prepared` with `params` at full source resolution; returns tightly-packed RGBA8
    /// (`w*h*4`). Thin identity wrapper over [`Self::render_to`] — byte-identical to the pre-viewport
    /// pipeline, so every existing caller / golden / export is unaffected.
    pub fn render(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
    ) -> Result<Vec<u8>, PipelineError> {
        let view = crate::params::ViewParams::full(prepared.width, prepared.height);
        let readback = self.readback_buffer(ctx, prepared)?;
        self.render_to(
            ctx,
            prepared,
            params,
            &view,
            None,
            &prepared.output,
            readback,
            prepared.bpr,
            prepared.width,
            prepared.height,
        )
    }

    /// Render only the crop-local viewport window `view` into a `view.out_w × view.out_h` target
    /// (the RapidRAW pattern: full-res source cached once, small viewport-sized output per edit).
    /// Returns tightly-packed RGBA8 (`out_w*out_h*4`). An identity, source-sized, overlay-off view
    /// reuses the prepared (source-sized) target and is byte-identical to [`Self::render`]; any other
    /// view renders into a transient display-sized target (the render thread owns a reusable one —
    /// Workstream B).
    pub fn render_view(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &crate::params::ViewParams,
    ) -> Result<Vec<u8>, PipelineError> {
        self.render_view_inner(ctx, prepared, params, view, None)
    }

    /// Render with the AI coverage snapshot belonging to this request. Coverage is consumed under
    /// the same render lock as the uniforms and mask pre-pass, so concurrent viewport/histogram
    /// requests cannot replace one another's masks.
    pub fn render_view_with_ai_coverages(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &crate::params::ViewParams,
        ai_coverages: &[AiCoverage],
    ) -> Result<Vec<u8>, PipelineError> {
        self.render_view_inner(ctx, prepared, params, view, Some(ai_coverages))
    }

    fn render_view_inner(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &crate::params::ViewParams,
        ai_coverages: Option<&[AiCoverage]>,
    ) -> Result<Vec<u8>, PipelineError> {
        // Keep the existing clamped viewport contract; IPC callers clamp identically when framing
        // the returned buffer. Validate the padded byte size against the separate buffer limit.
        let limits = ctx.device.limits();
        let ow = view.out_w.clamp(1, limits.max_texture_dimension_2d);
        let oh = view.out_h.clamp(1, limits.max_texture_dimension_2d);
        crate::validation::output_layout(
            ow,
            oh,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;
        if !view.active && view.overlay_layer < 0 && ow == prepared.width && oh == prepared.height {
            let readback = self.readback_buffer(ctx, prepared)?;
            return self.render_to(
                ctx,
                prepared,
                params,
                view,
                ai_coverages,
                &prepared.output,
                readback,
                prepared.bpr,
                ow,
                oh,
            );
        }
        // Poison policy: RECOVER + RESET. The target is disposable and re-created below whenever the
        // size differs; dropping it on poison just forces that re-creation, so a half-built target
        // from a panicked render is never reused.
        let mut target = prepared.view_target.lock().unwrap_or_else(|p| {
            let mut g = p.into_inner();
            *g = None;
            g
        });
        if target
            .as_ref()
            .is_none_or(|t| t.width != ow || t.height != oh)
        {
            *target = Some(ViewTarget::new(&ctx.device, ow, oh)?);
        }
        let target = target.as_ref().expect("viewport target initialized");
        self.render_to(
            ctx,
            prepared,
            params,
            view,
            ai_coverages,
            &target.texture,
            &target.readback,
            target.bpr,
            ow,
            oh,
        )
    }

    /// Develop `prepared` into a caller-owned texture (format `Rgba8Unorm`, usage must include
    /// RENDER_ATTACHMENT) at exactly `view.out_w × view.out_h`. Submits the work and returns the
    /// submission index; does NOT read back — the developed pixels stay on the GPU (sampled by
    /// egui for interactive native presentation). The caller owns the texture's lifetime and must
    /// not reuse it while a previous submission that targets it may still be sampled
    /// (renderer-side leasing).
    ///
    /// Uniform lifetime: uniforms live in `PreparedImage`'s shared buffers and are rewritten with
    /// `queue.write_buffer` before each submit. wgpu applies `write_buffer` at the next `submit`,
    /// so an already-submitted frame keeps the values it was submitted with — back-to-back
    /// submissions without waiting in between each render their own params (proven by
    /// `tests/texture_target.rs::back_to_back_submissions_keep_their_own_uniforms`).
    ///
    /// Validation failures (format / usage / size vs `view`) return
    /// [`PipelineError::InvalidTarget`]; never panic. Takes `prepared`'s render lock for
    /// encode + submit (no readback tail, so none is needed past the submit).
    pub fn render_view_into_texture(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        params: &DevelopParams,
        view: &crate::params::ViewParams,
        ai_coverages: Option<&[AiCoverage]>,
        target: &wgpu::Texture,
    ) -> Result<wgpu::SubmissionIndex, PipelineError> {
        validate_render_target(&ctx.device, target, view)?;
        // One render at a time per image — shared uniforms/LUT/mask layers (same poison policy as
        // `render_to`).
        let _render_guard = prepared
            .render_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let enc = self.encode_develop(
            ctx,
            prepared,
            params,
            view,
            ai_coverages,
            &target_view,
            (view.out_w, view.out_h),
        )?;
        Ok(ctx.queue.submit(Some(enc.finish())))
    }

    /// Explicit CPU readback of a texture produced by [`Self::render_view_into_texture`] (tests,
    /// export, diagnostics). Blocking: submits the copy and waits for the map. Returns
    /// tightly-packed RGBA8 (`width*height*4`). The texture must be `Rgba8Unorm`, single-sampled
    /// 2D, with `COPY_SRC` usage; mismatches return [`PipelineError::InvalidTarget`].
    pub fn read_back_texture(
        &self,
        ctx: &GpuContext,
        texture: &wgpu::Texture,
    ) -> Result<Vec<u8>, PipelineError> {
        let (ow, oh) = (texture.width(), texture.height());
        let limits = ctx.device.limits();
        if texture.format() != wgpu::TextureFormat::Rgba8Unorm {
            return Err(PipelineError::InvalidTarget(format!(
                "readback format {:?} is not Rgba8Unorm",
                texture.format()
            )));
        }
        if !texture.usage().contains(wgpu::TextureUsages::COPY_SRC) {
            return Err(PipelineError::InvalidTarget(
                "readback target lacks COPY_SRC usage".to_string(),
            ));
        }
        if texture.dimension() != wgpu::TextureDimension::D2
            || texture.mip_level_count() != 1
            || texture.sample_count() != 1
        {
            return Err(PipelineError::InvalidTarget(format!(
                "readback target must be a single-sampled 2D texture (dimension {:?}, {} mips, {} samples)",
                texture.dimension(),
                texture.mip_level_count(),
                texture.sample_count()
            )));
        }
        let (bpr, readback_size) = crate::validation::output_layout(
            ow,
            oh,
            limits.max_texture_dimension_2d,
            limits.max_buffer_size,
        )?;
        let readback = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("develop-texture-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("develop-texture-readback-enc"),
            });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(bpr),
                    rows_per_image: Some(oh),
                },
            },
            wgpu::Extent3d {
                width: ow,
                height: oh,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(enc.finish()));
        read_back_mapped(ctx, &readback, bpr, ow, oh)
    }

    /// Bake a mask's brush strokes into the per-image brush scratch texture. Clears first, then
    /// draws each stroke's instanced dabs in order (paint = MAX blend, erase = multiply).
    fn bake_brush(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        mres: &MaskResources,
        mask: &crate::params::Mask,
    ) {
        let device = &ctx.device;
        let strokes = crate::mask::mask_brush_strokes(mask);
        let batches = crate::mask::flatten_strokes(&strokes);
        let view = mres
            .brush_tex
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Clear pass (runs even with no strokes, so stale coverage never leaks in).
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("brush-clear-enc"),
        });
        {
            let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("brush-clear"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        ctx.queue.submit(Some(enc.finish()));

        for batch in &batches {
            if batch.count == 0 {
                continue;
            }
            let inst = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("brush-instances"),
                contents: &batch.instances,
                usage: wgpu::BufferUsages::VERTEX,
            });
            let mut benc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("brush-bake-enc"),
            });
            {
                let mut pass = benc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("brush-bake"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(if batch.is_erase {
                    &self.brush_bake.erase
                } else {
                    &self.brush_bake.paint
                });
                pass.set_bind_group(0, &prepared.bake_bind, &[]);
                pass.set_vertex_buffer(0, inst.slice(..));
                pass.draw(0..6, 0..batch.count);
            }
            ctx.queue.submit(Some(benc.finish()));
        }
    }

    /// One edge-aware refine pass: read `src`, write `dst`. `dir`=(1,0)/(0,1) selects the separable
    /// axis; `sigma_px`=0 makes it a plain copy.
    #[allow(clippy::too_many_arguments)]
    fn refine_pass(
        &self,
        ctx: &GpuContext,
        prepared: &PreparedImage,
        src: &wgpu::TextureView,
        dst: &wgpu::TextureView,
        dir: [f32; 2],
        sigma_px: f32,
        luma_sigma: f32,
        texel: [f32; 2],
    ) {
        let device = &ctx.device;
        ctx.queue.write_buffer(
            &prepared.refine_uniform,
            0,
            bytemuck::bytes_of(&crate::mask::RefineUniform {
                dir,
                sigma_px,
                luma_sigma,
                texel,
                _pad: [0.0, 0.0],
            }),
        );
        let input_view = prepared
            ._input
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask-refine-bg"),
            layout: &self.refine.bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.mask_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&input_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: prepared.refine_uniform.as_entire_binding(),
                },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mask-refine-enc"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("mask-refine"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.refine.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        ctx.queue.submit(Some(enc.finish()));
    }

    /// One-shot convenience: prepare + render (used for full-res export).
    pub fn render_once(
        &self,
        ctx: &GpuContext,
        img: &LinearImage,
        params: &DevelopParams,
    ) -> Result<Vec<u8>, PipelineError> {
        let prepared = self.prepare(ctx, img)?;
        self.render(ctx, &prepared, params)
    }

    /// Render the complete selected crop at native pixel dimensions. Unlike a source-sized
    /// letterbox followed by a pixel cut, this preserves both crop area and quarter-turn axes.
    pub fn render_cropped_with_ai_coverages(
        &self,
        ctx: &GpuContext,
        image: &LinearImage,
        params: &DevelopParams,
        coverages: &[AiCoverage],
    ) -> Result<(Vec<u8>, u32, u32), PipelineError> {
        let prepared = self.prepare(ctx, image)?;
        let (width, height) = params.crop.output_size(image.width, image.height);
        let view = crate::params::ViewParams {
            active: true,
            ..crate::params::ViewParams::full(width, height)
        };
        let rgba = self.render_view_with_ai_coverages(ctx, &prepared, params, &view, coverages)?;
        Ok((rgba, width, height))
    }
}

#[cfg(test)]
mod mip_tests {
    use super::downsample_rgba_f32;

    fn gray(values: &[f32]) -> Vec<f32> {
        values.iter().flat_map(|&v| [v, v, v, 1.0]).collect()
    }

    #[test]
    fn odd_axes_include_the_last_texel() {
        for (w, h) in [(3, 1), (1, 3)] {
            assert_eq!(
                downsample_rgba_f32(&gray(&[0.0, 0.0, 1.0]), w, h),
                gray(&[1.0 / 3.0])
            );
        }
        // 5 -> 2 footprints have width 2.5: weights (2,2,1)/5 and (1,2,2)/5.
        assert_eq!(
            downsample_rgba_f32(&gray(&[0.0, 0.0, 1.0, 0.0, 1.0]), 5, 1),
            gray(&[0.2, 0.6])
        );
    }

    #[test]
    fn odd_2d_mips_preserve_mean_including_bottom_right_corner() {
        let mut values = [0.0; 15];
        values[14] = 1.0;
        let next = downsample_rgba_f32(&gray(&values), 5, 3);
        assert_eq!(next, gray(&[0.0, 2.0 / 15.0]));
        let last = downsample_rgba_f32(&next, 2, 1);
        assert_eq!(last, gray(&[1.0 / 15.0]));
    }

    #[test]
    fn constant_hdr_and_negative_channels_are_preserved() {
        let color = [-0.125, 3.0, 32.0, 1.0];
        for (w, h) in [(1, 1), (5, 3), (3, 4), (4, 4)] {
            let source = color.repeat((w * h) as usize);
            let result = downsample_rgba_f32(&source, w, h);
            assert_eq!(
                result,
                color.repeat(((w / 2).max(1) * (h / 2).max(1)) as usize)
            );
        }
        // An average of finite values must not overflow just because the sum did.
        for (w, h) in [(3, 3), (2, 2)] {
            let source = [f32::MAX, f32::MAX, f32::MAX, 1.0].repeat((w * h) as usize);
            assert_eq!(
                downsample_rgba_f32(&source, w, h),
                [f32::MAX, f32::MAX, f32::MAX, 1.0]
            );
        }
    }

    #[test]
    fn even_dimensions_keep_the_existing_two_by_two_box_filter() {
        let values: Vec<f32> = (0..16).map(|v| v as f32).collect();
        assert_eq!(
            downsample_rgba_f32(&gray(&values), 4, 4),
            gray(&[2.5, 4.5, 10.5, 12.5])
        );
    }

    #[test]
    fn mip_mean_matches_source_for_varied_odd_and_even_sizes() {
        for w in 1..=9 {
            for h in 1..=9 {
                let values: Vec<f32> = (0..w * h)
                    .map(|i| (i * 17 % 31) as f32 / 7.0 - 2.0)
                    .collect();
                let expected =
                    values.iter().map(|&v| f64::from(v)).sum::<f64>() / values.len() as f64;
                let mip = downsample_rgba_f32(&gray(&values), w, h);
                let actual = mip.chunks_exact(4).map(|p| f64::from(p[0])).sum::<f64>()
                    / (mip.len() / 4) as f64;
                assert!(
                    (actual - expected).abs() < 1e-6,
                    "{w}x{h}: {actual} vs {expected}"
                );
            }
        }
    }
}
