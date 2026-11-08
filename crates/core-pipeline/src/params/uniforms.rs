use super::*;
/// GPU uniform layout (std140-friendly: 48 bytes, three 16-byte rows).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ParamsUniform {
    pub wb_gain: [f32; 3],
    pub exposure: f32,
    pub saturation: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub blacks: f32,
    pub whites: f32,
    pub _pad0: f32,
    pub _pad1: f32,
}
/// One mask's scalar deltas, packed for the develop storage buffer. std430-clean: 48 bytes
/// (three 16-byte rows). `wb_gain` is the multiplicative delta gain (≈1.0 at temp/tint=0); the
/// other scalars are additive deltas normalized to the same [-1,1] convention as `ParamsUniform`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaskParamsUniform {
    pub wb_gain: [f32; 3],
    pub exposure: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub blacks: f32,
    pub whites: f32,
    pub opacity: f32,
    /// 1.0 for a packed (enabled) mask. Always 1.0 today; kept for a per-layer shader guard.
    pub enabled: f32,
}

/// The develop storage buffer: a count plus a fixed `MASK_CAP` array of per-mask deltas.
/// 16 + 16*48 = 784 bytes. Bound as a read-only storage buffer (std430) in `develop.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MaskBufferUniform {
    pub count: u32,
    pub _pad: [u32; 3],
    pub masks: [MaskParamsUniform; MASK_CAP],
}

impl Default for MaskBufferUniform {
    fn default() -> Self {
        DevelopParams::default().to_mask_buffer()
    }
}

/// Secondary GPU uniform for the effects that don't belong in the guarded `ParamsUniform`.
/// std140-clean: an array of `vec4` rows (16-byte aligned). 128 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FxUniform {
    /// Per-hue band: `vec4(hue_shift, sat, lum, _pad)`, each in [-1,1].
    pub hsl: [[f32; 4]; HSL_BANDS],
}

impl Default for FxUniform {
    fn default() -> Self {
        DevelopParams::default().to_fx()
    }
}

/// Global white-balance CAT matrix for the GPU. std140 mat3 = three 16-byte-aligned `vec4` columns
/// (48 bytes). The `.xyz` of each column feeds a `mat3x3` in `develop.wgsl` (`@binding(8)`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct WbUniform {
    pub cols: [[f32; 4]; 3],
}

impl Default for WbUniform {
    fn default() -> Self {
        DevelopParams::default().to_wb_uniform()
    }
}

/// Detail (sharpen / luma+color NR) + Lens (vignette) scalars + the image texel size + Presence
/// (clarity/texture/dehaze), for the GPU. std140-clean: three `vec4` rows (48 bytes). `@binding(9)`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExtraUniform {
    /// (sharpen 0..1.5, nr_luma 0..1, nr_color 0..1, vignette -1..1).
    pub detail: [f32; 4],
    /// (1/width, 1/height, baseline_gain, _).
    pub texel: [f32; 4],
    /// (clarity -1..1, texture -1..1, dehaze -1..1, active 0/1).
    pub local: [f32; 4],
}

impl Default for ExtraUniform {
    fn default() -> Self {
        DevelopParams::default().to_extra([0.0, 0.0])
    }
}

/// Scene-referred base tone operator uniform (`@binding(10)`). std140-clean: one `vec4` (16 bytes).
/// `params` = (u_min, u_max, hue_protect, _pad). The curve itself rides the base-curve LUT texture
/// (`@binding(11)`); see `build_base_curve_lut`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ToneOpUniform {
    pub params: [f32; 4],
}

impl Default for ToneOpUniform {
    fn default() -> Self {
        DevelopParams::default().to_tone_op()
    }
}

/// Crop + straighten geometry uniform (`@binding(12)`). std140-clean: four `vec4` (64 bytes).
/// `crop` = (cx, cy, hw, hh); `rot` = (cos θ, sin θ, autozoom, active); `aspect` = (src W/H, out W/H,
/// lens-active, _); `lens` = (k1, k2, ca_red, ca_blue). See `to_geom` / `geom_src_uv` / `geom_lens_uv`
/// and `geom_resolve` / `lens_sample` in develop.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GeomUniform {
    pub crop: [f32; 4],
    pub rot: [f32; 4],
    pub aspect: [f32; 4],
    pub lens: [f32; 4],
}

impl Default for GeomUniform {
    fn default() -> Self {
        DevelopParams::default().to_geom(1.0, 1.0)
    }
}

/// Viewport + mask-overlay uniform (`@binding(13)`). std140-clean: three `vec4` (48 bytes).
/// `rect`  = (origin.x, origin.y, size.x, size.y) — the visible window in **crop-local uv** [0,1]
///           (zoom/pan); `flags` = (active 0/1, overlay_layer as f32 (-1 = off), overlay_strength,
///           crop-preview 0/1); `color` = (overlay r, g, b, _pad) in display sRGB.
///
/// `active = 0` (the `ViewParams::full` identity) makes the shader take the legacy `geom_resolve`
/// path and leaves the overlay branch dead, so a full-frame render is **byte-identical** to one
/// produced before this binding existed. The golden suites + export rely on that.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ViewUniform {
    pub rect: [f32; 4],
    pub flags: [f32; 4],
    pub color: [f32; 4],
}

impl Default for ViewUniform {
    fn default() -> Self {
        ViewParams::full(1, 1).to_uniform()
    }
}

/// Color-balance-RGB grading uniform (`@binding(14)`). std140-clean: five `vec4` (80 bytes). Each of
/// `global/shadows/midtones/highlights` is a grading-RGB vector in `.xyz` (`.w` spare); `params` =
/// (contrast, saturation, active 0/1, _pad). `active = 0` ⇒ the shader skips the grading-space round
/// trip, so a default render is **byte-identical** to a pre-binding-14 build (golden + export rely on
/// it). The ProPhoto⇄grading matrices live in `develop.wgsl` as constants (see `grading_matrices`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct CbRgbUniform {
    /// ProPhoto(D50) → grading-RGB(D65), std140 mat3 (3 × `vec4` columns).
    pub fwd: [[f32; 4]; 3],
    /// grading-RGB(D65) → ProPhoto(D50).
    pub inv: [[f32; 4]; 3],
    pub global: [f32; 4],
    pub shadows: [f32; 4],
    pub midtones: [f32; 4],
    pub highlights: [f32; 4],
    pub params: [f32; 4],
}

impl Default for CbRgbUniform {
    fn default() -> Self {
        DevelopParams::default().to_cbrgb()
    }
}

/// Channel-mixer uniform (`@binding(15)`). std140-clean: four `vec4` (64 bytes). `r/g/b` hold each
/// output channel's source-RGB weights in `.xyz` (`.w` spare); `params` = (active 0/1, _, _, _).
/// `active = 0` ⇒ the shader passes the color through untouched, so a default render is
/// **byte-identical** to a pre-binding-15 build (golden + export rely on it).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ChanMixUniform {
    pub r: [f32; 4],
    pub g: [f32; 4],
    pub b: [f32; 4],
    pub params: [f32; 4],
}

impl Default for ChanMixUniform {
    fn default() -> Self {
        DevelopParams::default().to_chanmix()
    }
}
