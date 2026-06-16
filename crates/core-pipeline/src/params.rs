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

impl DevelopParams {
    /// Map a -100..100 white-balance temp/tint pair to per-channel linear gains.
    fn wb_gain(&self) -> [f32; 3] {
        let t = (self.temp / 100.0).clamp(-1.0, 1.0);
        let g = (self.tint / 100.0).clamp(-1.0, 1.0);
        // Warm: boost R, cut B. Tint+ (magenta): boost R&B, cut G slightly.
        [
            1.0 + 0.45 * t + 0.10 * g,
            1.0 - 0.10 * g,
            1.0 - 0.45 * t + 0.10 * g,
        ]
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
