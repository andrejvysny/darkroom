use serde::{Deserialize, Serialize};

/// Maximum number of masks per image (UI + GPU storage/array-layer cap). Matches Lightroom's
/// practical ceiling and fits trivially in the alpha texture-array and the storage buffer.
pub const MASK_CAP: usize = 16;

/// The local adjustment set a mask carries. Same scalar vocabulary as the global develop, but
/// applied sequentially on top of the running develop where the mask's alpha is nonzero.
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

/// One brush stroke: a poly-bezier path (control points in normalized `[0,1]` image coords) plus the
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

/// One prompt point for an AI (SAM) mask, in normalized `[0,1]` image coords. `positive=false` is a
/// subtract/background click. The point list is the persisted, re-editable definition of the
/// selection (Lightroom keeps AI selections editable); the baked alpha is cached out-of-band.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AiPoint {
    pub x: f32,
    pub y: f32,
    pub positive: bool,
}

impl Default for AiPoint {
    fn default() -> Self {
        Self {
            x: 0.5,
            y: 0.5,
            positive: true,
        }
    }
}

/// The shape/source of a mask component. Coverage is computed in the mask pre-pass; only the
/// per-mask scalar deltas reach the develop shader. `Ai` coverage is supplied out-of-band as a baked
/// alpha (`AiCoverage`) keyed by the component's `hash`; the pre-pass samples it for kind==5.
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
    /// Luminance range selection (scene-linear luma), `lo`/`hi` in `[0,1]`, `feather` ramp width.
    LuminanceRange { lo: f32, hi: f32, feather: f32 },
    /// Color range selection around a sampled hue/saturation, `tol`/`feather` in `[0,1]`.
    ColorRange {
        hue: f32,
        sat: f32,
        tol: f32,
        feather: f32,
    },
    /// AI/promptable mask (interactive object select via SAM; later Subject/Sky/…). `model` names the
    /// producing model; `points` are the re-editable prompt clicks; `hash` keys the baked `AiCoverage`
    /// the backend computed for these prompts (empty until first computed).
    Ai {
        model: String,
        #[serde(default)]
        points: Vec<AiPoint>,
        #[serde(default)]
        hash: String,
    },
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
