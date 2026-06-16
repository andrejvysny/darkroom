//! Develop parameters (the editable per-image adjustment set) + GPU uniform packing.

use serde::{Deserialize, Serialize};

/// A single tone-curve control point in normalized [0,1] (x = input, y = output).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CurvePoint {
    pub x: f32,
    pub y: f32,
}

/// Per-channel tone curves. Empty (or <2 points) on a channel means identity (no-op).
/// `rgb` is the master curve applied first; `r`/`g`/`b` are then applied per channel.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ToneCurve {
    pub rgb: Vec<CurvePoint>,
    pub r: Vec<CurvePoint>,
    pub g: Vec<CurvePoint>,
    pub b: Vec<CurvePoint>,
}

impl ToneCurve {
    /// True when every channel is identity (so the LUT stage can be skipped).
    pub fn is_identity(&self) -> bool {
        [&self.rgb, &self.r, &self.g, &self.b]
            .iter()
            .all(|c| crate::curve::is_identity(c))
    }
}

/// Number of hue bands in the HSL/color mixer (red, orange, yellow, green, aqua, blue,
/// purple, magenta). Band centers live in `develop.wgsl` (`HUE_CENTERS`).
pub const HSL_BANDS: usize = 8;

/// One hue band's hue/saturation/luminance adjustment, each -100..100. All zero = no-op.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HslBand {
    pub h: f32,
    pub s: f32,
    pub l: f32,
}

/// Maximum number of masks per image (UI + GPU storage/array-layer cap). Matches Lightroom's
/// practical ceiling and fits trivially in the alpha texture-array and the storage buffer.
pub const MASK_CAP: usize = 16;

/// The local adjustment set a mask carries. Same scalar vocabulary as the global develop, but
/// interpreted as DELTAS applied on top of the base develop where the mask's alpha is nonzero.
/// All default to zero (no-op).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalAdjust {
    pub exposure: f32,
    pub temp: f32,
    pub tint: f32,
    pub contrast: f32,
    pub saturation: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub blacks: f32,
    pub whites: f32,
}

/// One brush stroke: a poly-bezier path (control points in normalized [0,1] image coords) plus the
/// per-stroke brush settings. Rasterized into the mask's alpha buffer in the GPU brush-bake pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct BrushStroke {
    /// Bezier control points, normalized to the longest edge (resolution-independent).
    pub points: Vec<[f32; 2]>,
    /// Brush radius as a fraction of the longest edge.
    pub size: f32,
    /// Edge falloff, 0 (soft) .. 1 (hard).
    pub hardness: f32,
    /// Per-stamp alpha contribution, 0..1.
    pub flow: f32,
    /// Per-stroke max alpha, 0..1.
    pub opacity: f32,
    /// Erase (subtract) instead of paint.
    pub is_erase: bool,
}

/// How a component combines with the running per-mask alpha.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum MaskOp {
    #[default]
    Add, // union: alpha = max(alpha, a)
    Subtract,  // alpha = alpha * (1 - a)
    Intersect, // alpha = alpha * a
}

/// The shape/source of a mask component. Coverage is computed in the mask pre-pass; only the
/// per-mask scalar deltas reach the develop shader. `Ai` is schema-only groundwork (not implemented):
/// it shares the Brush sampling path, so a future model just writes the alpha layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ComponentKind {
    /// Graduated/linear gradient: full effect at `p0`, none at `p1` (normalized coords).
    Linear { p0: [f32; 2], p1: [f32; 2] },
    /// Elliptical radial mask. `center`/`radius` normalized; `angle` radians; `feather` 0..1 inward.
    Radial {
        center: [f32; 2],
        radius: [f32; 2],
        angle: f32,
        feather: f32,
    },
    /// Paint/brush coverage.
    Brush { strokes: Vec<BrushStroke> },
    /// Luminance range selection (scene-linear luma), `lo`/`hi` in [0,1], `feather` ramp width.
    LuminanceRange { lo: f32, hi: f32, feather: f32 },
    /// Color range selection around a sampled hue/saturation, `tol`/`feather` in [0,1].
    ColorRange {
        hue: f32,
        sat: f32,
        tol: f32,
        feather: f32,
    },
    /// Future AI/semantic mask (Select Subject/Sky, SAM, …). Schema only; no coverage yet.
    Ai { model: String },
}

/// One component of a mask: a shape/source, how it combines, and whether it's inverted.
/// `feather` requests guided-filter edge-aware refinement (brush/range only; parametric is smooth).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct MaskComponent {
    pub kind: ComponentKind,
    pub op: MaskOp,
    pub invert: bool,
    pub feather: bool,
}

impl Default for MaskComponent {
    fn default() -> Self {
        Self {
            kind: ComponentKind::Linear {
                p0: [0.0, 0.0],
                p1: [0.0, 1.0],
            },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }
    }
}

/// A single mask: an ordered list of components composited (Add/Subtract/Intersect) into one alpha
/// buffer, carrying one shared local adjustment set, a global opacity, and an enabled flag.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Mask {
    pub name: String,
    pub components: Vec<MaskComponent>,
    pub adjust: LocalAdjust,
    pub opacity: f32,
    pub enabled: bool,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            name: String::new(),
            components: Vec::new(),
            adjust: LocalAdjust::default(),
            opacity: 1.0,
            enabled: true,
        }
    }
}

/// Editable adjustments applied to the cached linear buffer. All default to a no-op.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DevelopParams {
    /// Exposure in stops (EV). 0 = unchanged.
    pub exposure: f32,
    /// White-balance temperature tweak, -100..100 (warm +).
    pub temp: f32,
    /// White-balance tint tweak, -100..100 (magenta +).
    pub tint: f32,
    /// Contrast, -100..100 (display-space pivot at mid-gray).
    pub contrast: f32,
    /// Saturation, -100..100.
    pub saturation: f32,
    /// Highlight recovery/boost, -100..100 (negative recovers).
    pub highlights: f32,
    /// Shadow lift/crush, -100..100 (positive lifts).
    pub shadows: f32,
    /// Black point, -100..100.
    pub blacks: f32,
    /// White point, -100..100.
    pub whites: f32,
    /// Tone curve (display-space LUT). Identity by default.
    pub tone_curve: ToneCurve,
    /// Per-hue HSL mixer (8 bands). All-zero by default.
    pub hsl: [HslBand; HSL_BANDS],
    /// Local adjustment masks, in stacking order. Empty by default (v1 edits deserialize here).
    #[serde(default)]
    pub masks: Vec<Mask>,
}

impl Default for DevelopParams {
    fn default() -> Self {
        Self {
            exposure: 0.0,
            temp: 0.0,
            tint: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            blacks: 0.0,
            whites: 0.0,
            tone_curve: ToneCurve::default(),
            hsl: [HslBand::default(); HSL_BANDS],
            masks: Vec::new(),
        }
    }
}

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

/// Map a -100..100 white-balance temp/tint pair to per-channel linear gains.
/// Free function so both the global params and per-mask deltas share the exact same math.
pub(crate) fn wb_gain_from(temp: f32, tint: f32) -> [f32; 3] {
    let t = (temp / 100.0).clamp(-1.0, 1.0);
    let g = (tint / 100.0).clamp(-1.0, 1.0);
    // Warm: boost R, cut B. Tint+ (magenta): boost R&B, cut G slightly.
    [
        1.0 + 0.45 * t + 0.10 * g,
        1.0 - 0.10 * g,
        1.0 - 0.45 * t + 0.10 * g,
    ]
}

impl DevelopParams {
    /// Map a -100..100 white-balance temp/tint pair to per-channel linear gains.
    fn wb_gain(&self) -> [f32; 3] {
        wb_gain_from(self.temp, self.tint)
    }

    pub fn to_uniform(&self) -> ParamsUniform {
        ParamsUniform {
            wb_gain: self.wb_gain(),
            exposure: self.exposure,
            saturation: (self.saturation / 100.0).clamp(-1.0, 1.0),
            contrast: (self.contrast / 100.0).clamp(-1.0, 1.0),
            highlights: (self.highlights / 100.0).clamp(-1.0, 1.0),
            shadows: (self.shadows / 100.0).clamp(-1.0, 1.0),
            blacks: (self.blacks / 100.0).clamp(-1.0, 1.0),
            whites: (self.whites / 100.0).clamp(-1.0, 1.0),
            _pad0: 0.0,
            _pad1: 0.0,
        }
    }

    /// True when the HSL mixer is a no-op (lets the shader skip the RGB↔HSV round-trip cheaply
    /// via the uniform, but kept here for clarity/tests).
    pub fn hsl_is_identity(&self) -> bool {
        self.hsl
            .iter()
            .all(|b| b.h == 0.0 && b.s == 0.0 && b.l == 0.0)
    }

    /// Pack the per-hue HSL bands into the secondary GPU uniform (each band normalized to [-1,1]
    /// as `vec4(hue, sat, lum, _pad)`). Separate from `ParamsUniform` so the guarded std140
    /// `wb_gain` layout is never touched.
    pub fn to_fx(&self) -> FxUniform {
        let mut hsl = [[0.0f32; 4]; HSL_BANDS];
        for (i, b) in self.hsl.iter().enumerate() {
            hsl[i] = [
                (b.h / 100.0).clamp(-1.0, 1.0),
                (b.s / 100.0).clamp(-1.0, 1.0),
                (b.l / 100.0).clamp(-1.0, 1.0),
                0.0,
            ];
        }
        FxUniform { hsl }
    }

    /// Pack the enabled masks' scalar deltas into the develop storage buffer. Geometry/components do
    /// NOT go here — they drive the mask pre-pass which writes the alpha texture-array. Enabled masks
    /// are packed densely (layers 0..count) in stacking order; the pre-pass must use the same order.
    pub fn to_mask_buffer(&self) -> MaskBufferUniform {
        let mut masks = [bytemuck::Zeroable::zeroed(); MASK_CAP];
        let mut count = 0usize;
        for m in self.masks.iter().filter(|m| m.enabled).take(MASK_CAP) {
            let a = &m.adjust;
            masks[count] = MaskParamsUniform {
                wb_gain: wb_gain_from(a.temp, a.tint),
                exposure: a.exposure,
                contrast: (a.contrast / 100.0).clamp(-1.0, 1.0),
                saturation: (a.saturation / 100.0).clamp(-1.0, 1.0),
                highlights: (a.highlights / 100.0).clamp(-1.0, 1.0),
                shadows: (a.shadows / 100.0).clamp(-1.0, 1.0),
                blacks: (a.blacks / 100.0).clamp(-1.0, 1.0),
                whites: (a.whites / 100.0).clamp(-1.0, 1.0),
                opacity: m.opacity.clamp(0.0, 1.0),
                enabled: 1.0,
            };
            count += 1;
        }
        MaskBufferUniform {
            count: count as u32,
            _pad: [0; 3],
            masks,
        }
    }
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
