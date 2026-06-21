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

/// Crop + straighten geometry. Crop rect is center (`cx`,`cy`) + half-extents (`hw`,`hh`) in
/// normalized image coords; `angle` is the straighten correction in degrees (CCW+). Default is the
/// full frame, no rotation (identity). Applied as a UV-remap in the develop shader (see `to_geom`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Crop {
    pub cx: f32,
    pub cy: f32,
    pub hw: f32,
    pub hh: f32,
    pub angle: f32,
}

impl Default for Crop {
    fn default() -> Self {
        Self {
            cx: 0.5,
            cy: 0.5,
            hw: 0.5,
            hh: 0.5,
            angle: 0.0,
        }
    }
}

impl Crop {
    /// True when the crop is the full frame with no rotation (shader takes the identity fast-path).
    pub fn is_identity(&self) -> bool {
        self.cx == 0.5 && self.cy == 0.5 && self.hw >= 0.5 && self.hh >= 0.5 && self.angle == 0.0
    }

    /// The cropped content's pixel rect `(x, y, w, h)` within an input-sized (`w`×`h`) render. The
    /// develop shader letterbox-fits the crop centered into the input-sized output at exactly the
    /// crop's pixel dimensions, so cropping this rect (a plain pixel copy, no resize) yields the
    /// true-dimension export at full quality. Mirrors the letterbox math in `geom_resolve` for
    /// `out_aspect == src_aspect`.
    pub fn export_rect(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        if self.is_identity() {
            return (0, 0, w, h);
        }
        let src_aspect = w as f32 / h as f32;
        let ac = (self.hw / self.hh) * src_aspect;
        let (cwf, chf) = if src_aspect > ac {
            (ac / src_aspect, 1.0)
        } else {
            (1.0, src_aspect / ac)
        };
        let cw = (cwf * w as f32).round().clamp(1.0, w as f32) as u32;
        let ch = (chf * h as f32).round().clamp(1.0, h as f32) as u32;
        ((w - cw) / 2, (h - ch) / 2, cw, ch)
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
    /// Sharpening amount, 0..150 (unsharp mask). 0 = off.
    pub sharpen: f32,
    /// Luminance noise reduction, 0..100. 0 = off.
    pub nr_luma: f32,
    /// Color noise reduction, 0..100. 0 = off.
    pub nr_color: f32,
    /// Lens vignette, -100..100 (negative darkens corners, positive brightens).
    pub vignette: f32,
    /// Scene-referred base tone operator strength, 0..100. 0 = neutral soft-clip (flat), 100 = full
    /// ACR-matched contrast curve. Defaults to 100 so unedited images render with the ACR look.
    pub tone_amount: f32,
    /// Tone curve (display-space LUT). Identity by default.
    pub tone_curve: ToneCurve,
    /// Per-hue HSL mixer (8 bands). All-zero by default.
    pub hsl: [HslBand; HSL_BANDS],
    /// Crop + straighten geometry. Full-frame/no-rotation by default.
    #[serde(default)]
    pub crop: Crop,
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
            sharpen: 0.0,
            nr_luma: 0.0,
            nr_color: 0.0,
            vignette: 0.0,
            tone_amount: 100.0,
            tone_curve: ToneCurve::default(),
            hsl: [HslBand::default(); HSL_BANDS],
            crop: Crop::default(),
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
/// Free function so the per-mask WB deltas share the exact same math. (The GLOBAL WB now uses the
/// chromatic-adaptation matrix below; this gain remains the per-mask local-WB delta.)
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

// --- Global white balance: Planckian-locus target white + Bradford CAT (Phase 1c) -----------------
// All matrices are row-major `[[f64;3];3]`; computed in f64 and packed to f32 for the GPU. Kept
// self-contained (no rawler dep in core-pipeline); the ProPhoto constant mirrors rawler's
// `XYZ_TO_PROFOTORGB_D50` so this composes exactly with core-raw's ProPhoto working buffer.

type M3 = [[f64; 3]; 3];

/// XYZ → linear-ProPhoto (D50), identical to rawler's `XYZ_TO_PROFOTORGB_D50`.
const XYZ_TO_PROPHOTO_D50: M3 = [
    [1.3459433, -0.2556075, -0.0511118],
    [-0.5445989, 1.5081673, 0.0205351],
    [0.0, 0.0, 1.2118128],
];
/// Standard Bradford chromatic-adaptation cone-response matrix.
const BRADFORD: M3 = [
    [0.8951, 0.2664, -0.1614],
    [-0.7502, 1.7135, 0.0367],
    [0.0389, -0.0685, 1.0296],
];

fn mat3_mul(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}

fn mat3_vec(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

fn mat3_inv(m: &M3) -> M3 {
    let (a, b, c) = (m[0][0], m[0][1], m[0][2]);
    let (d, e, f) = (m[1][0], m[1][1], m[1][2]);
    let (g, h, i) = (m[2][0], m[2][1], m[2][2]);
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let inv_det = 1.0 / det;
    [
        [
            (e * i - f * h) * inv_det,
            (c * h - b * i) * inv_det,
            (b * f - c * e) * inv_det,
        ],
        [
            (f * g - d * i) * inv_det,
            (a * i - c * g) * inv_det,
            (c * d - a * f) * inv_det,
        ],
        [
            (d * h - e * g) * inv_det,
            (b * g - a * h) * inv_det,
            (a * e - b * d) * inv_det,
        ],
    ]
}

/// CCT (Kelvin) → CIE 1931 xy on the Planckian locus (Kim et al. 2002 cubic approximation).
fn kim_xy(t: f64) -> (f64, f64) {
    let (t2, t3) = (t * t, t * t * t);
    let x = if t >= 4000.0 {
        -3.0258469e9 / t3 + 2.1070379e6 / t2 + 0.2226347e3 / t + 0.240390
    } else {
        -0.2661239e9 / t3 - 0.2343589e6 / t2 + 0.8776956e3 / t + 0.179910
    };
    let (x2, x3) = (x * x, x * x * x);
    let y = if t >= 4000.0 {
        3.0817580 * x3 - 5.8733867 * x2 + 3.75112997 * x - 0.37001483
    } else if t >= 2222.0 {
        -0.9549476 * x3 - 1.37418593 * x2 + 2.09137015 * x - 0.16748867
    } else {
        -1.1063814 * x3 - 1.3481102 * x2 + 2.18555832 * x - 0.20219683
    };
    (x, y)
}

/// temp/tint (-100..100) → target white xy. temp+ = warmer (lower CCT, along the Planckian locus);
/// tint+ = magenta (lower y). Mired (reciprocal-temp) is ~perceptually uniform; the piecewise map
/// spans the full [83.33, 333.33] mired range symmetrically (no dead zone). Reference = the same
/// function at (0,0), so `wb_matrix(0,0)` is exactly identity.
const WB_BASE_MIRED: f64 = 153.85; // ~6500 K
fn white_xy(temp: f64, tint: f64) -> (f64, f64) {
    let t = temp / 100.0;
    let mired = if t >= 0.0 {
        WB_BASE_MIRED + t * (333.33 - WB_BASE_MIRED)
    } else {
        WB_BASE_MIRED + t * (WB_BASE_MIRED - 83.33)
    };
    let cct = (1.0e6 / mired).clamp(1667.0, 25000.0);
    let (x, mut y) = kim_xy(cct);
    y -= (tint / 100.0) * 0.04; // green↔magenta offset
    (x, y.max(1.0e-4))
}

fn xyy_to_xyz(x: f64, y: f64) -> [f64; 3] {
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// Bradford CAT (XYZ→XYZ) adapting the source white to the destination white.
fn bradford_cat(w_src: [f64; 3], w_dst: [f64; 3]) -> M3 {
    let ls = mat3_vec(&BRADFORD, w_src);
    let ld = mat3_vec(&BRADFORD, w_dst);
    let d: M3 = [
        [ld[0] / ls[0], 0.0, 0.0],
        [0.0, ld[1] / ls[1], 0.0],
        [0.0, 0.0, ld[2] / ls[2]],
    ];
    let b_inv = mat3_inv(&BRADFORD);
    mat3_mul(&mat3_mul(&b_inv, &d), &BRADFORD)
}

/// Global white-balance matrix for the linear-ProPhoto working space (row-major). At temp=tint=0 the
/// target white equals the reference white, so this is the exact identity (neutral untouched).
pub(crate) fn wb_matrix(temp: f32, tint: f32) -> [[f32; 3]; 3] {
    let (bx, by) = white_xy(0.0, 0.0);
    let w_ref = xyy_to_xyz(bx, by);
    let (tx, ty) = white_xy(temp as f64, tint as f64);
    let w_t = xyy_to_xyz(tx, ty);
    let cat = bradford_cat(w_ref, w_t);
    // M_pp = XYZ_TO_PP · CAT · PP_TO_XYZ (D50 throughout, matching the buffer + display matrices).
    let pp_to_xyz = mat3_inv(&XYZ_TO_PROPHOTO_D50);
    let m = mat3_mul(&mat3_mul(&XYZ_TO_PROPHOTO_D50, &cat), &pp_to_xyz);
    let mut out = [[0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            out[i][j] = m[i][j] as f32;
        }
    }
    out
}

// --- Scene-referred base tone operator (ACR-matched) ---------------------------------------------
// Replaces the old fixed `exp()` highlight shoulder. A monotone curve maps scene-linear ProPhoto
// [0,∞) → display-linear [0,1), applied per-channel before the ProPhoto→sRGB matrix (Codex review:
// per-channel on Rec.709 is unsafe for wide-gamut colors). Sampled into a LUT over the log-exposure
// domain (so headroom is covered) and uploaded to the GPU. `tone_amount` interpolates a neutral
// soft-clip (flat) → the ACR-seed curve; both anchor mid-grey 0.18 → 0.18 exactly. The ACR-seed
// shape is the analytic `x^p/(x^p+c)`; once reference ACR renders are fit, only the seed is replaced.

/// Base-curve LUT length (entries over the log-exposure domain). Matches `BASE_LUT_N` in develop.wgsl.
pub const BASE_LUT_SIZE: usize = 512;
/// Log-exposure domain (stops relative to mid-grey) the LUT spans. ~21 stops covers deep shadows to
/// far highlight headroom; all golden test inputs (x ≤ 8) map inside it.
const BASE_U_MIN: f32 = -13.0;
const BASE_U_MAX: f32 = 8.0;
/// Scene-linear mid-grey anchor (held fixed through the curve).
const MID_GREY: f32 = 0.18;
/// ACR-seed contrast exponent (Codex-recommended starting shape, pre-fit).
const ACR_P: f32 = 1.35;
/// Hue-protection strength: how strongly saturated highlights are pulled toward the (hue-preserving)
/// luminance-ratio variant vs the per-channel curve. Auto (not user-controlled) for now.
const BASE_HUE_PROTECT: f32 = 0.6;

/// The base tone operator value `f(x)` for one scalar channel, `amount` in [0,1].
/// Monotone, `f(0)=0`, `f(0.18)=0.18` for all amounts, asymptotes below 1.
pub fn base_curve_value(x: f32, amount: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    // Neutral soft-clip (Reinhard anchored at 0.18): f0(0.18) = 0.18/(0.18+0.82) = 0.18.
    let f0 = x / (x + (1.0 - MID_GREY) / MID_GREY * MID_GREY); // = x / (x + 0.82)
                                                               // ACR-seed: x^p / (x^p + c), with c chosen so f1(0.18) = 0.18 exactly.
    let xp = x.powf(ACR_P);
    let c = MID_GREY.powf(ACR_P - 1.0) * (1.0 - MID_GREY);
    let f1 = xp / (xp + c);
    let y = f0 + (f1 - f0) * amount.clamp(0.0, 1.0);
    y.max(0.0)
}

/// Sample the base tone operator into a LUT over the log-exposure domain (`BASE_U_MIN..BASE_U_MAX`).
pub fn build_base_curve_lut(amount: f32) -> Vec<f32> {
    let n = BASE_LUT_SIZE;
    let mut lut = vec![0f32; n];
    for (i, v) in lut.iter_mut().enumerate() {
        let u = BASE_U_MIN + (i as f32 / (n - 1) as f32) * (BASE_U_MAX - BASE_U_MIN);
        let x = MID_GREY * 2f32.powf(u);
        *v = base_curve_value(x, amount);
    }
    lut
}

// --- Crop + straighten geometry remap ------------------------------------------------------------
// The develop shader maps each output fragment's crop-local UV `u` ∈ [0,1] back to a source UV, so
// crop/straighten is one inverse-mapped sampling stage (Codex review). Rotation happens in
// PIXEL/aspect-correct space (rotating normalized UV would shear a non-square image). `src_aspect` =
// W/H of the source. These helpers mirror the WGSL exactly so the math is unit-testable on the CPU.

/// Auto-zoom factor (≥1) that keeps the rotated crop rectangle fully inside the source [0,1]², so a
/// straighten never samples past the image edge. Closed form from the Codex review.
pub fn geom_autozoom(c: &Crop, src_aspect: f32) -> f32 {
    let theta = c.angle.to_radians();
    let (ct, st) = (theta.cos().abs(), theta.sin().abs());
    // Max normalized displacement of the rotated footprint (hh·H/W = hh/aspect; hw·W/H = hw·aspect).
    let ax = ct * c.hw + st * c.hh / src_aspect;
    let ay = st * c.hw * src_aspect + ct * c.hh;
    let mx = c.cx.min(1.0 - c.cx).max(1e-6);
    let my = c.cy.min(1.0 - c.cy).max(1e-6);
    1.0_f32.max(ax / mx).max(ay / my)
}

/// Map a crop-local output UV `u` ∈ [0,1]² to a source UV (inverse rotation about the crop center,
/// pixel-space, then auto-zoom). Mirrors `geom_resolve` in develop.wgsl. `z` = `geom_autozoom`.
pub fn geom_src_uv(c: &Crop, src_aspect: f32, z: f32, u: [f32; 2]) -> [f32; 2] {
    let theta = c.angle.to_radians();
    let (ct, st) = (theta.cos(), theta.sin());
    let d = [(2.0 * u[0] - 1.0) * c.hw, (2.0 * u[1] - 1.0) * c.hh];
    let dpx = [d[0] * src_aspect, d[1]];
    // Inverse rotation R(-θ) in pixel space.
    let r = [ct * dpx[0] + st * dpx[1], -st * dpx[0] + ct * dpx[1]];
    let disp = [r[0] / src_aspect / z, r[1] / z];
    [c.cx + disp[0], c.cy + disp[1]]
}

impl DevelopParams {
    pub fn to_uniform(&self) -> ParamsUniform {
        ParamsUniform {
            // Global WB now rides the CAT matrix (`WbUniform`, binding 8); keep this neutral so the
            // shader's `apply_local_linear` is a no-op for the global wb_gain. Masks still use
            // `wb_gain_from` as a per-mask delta (see `to_mask_buffer`).
            wb_gain: [1.0, 1.0, 1.0],
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

    /// Pack the Detail (sharpen/NR) + Lens (vignette) scalars for the GPU. `texel` = (1/w, 1/h) of
    /// the image being rendered (for neighborhood sampling), supplied by the backend.
    pub fn to_extra(&self, texel: [f32; 2]) -> ExtraUniform {
        ExtraUniform {
            detail: [
                (self.sharpen / 100.0).max(0.0), // 0..1.5
                (self.nr_luma / 100.0).clamp(0.0, 1.0),
                (self.nr_color / 100.0).clamp(0.0, 1.0),
                (self.vignette / 100.0).clamp(-1.0, 1.0),
            ],
            texel: [texel[0], texel[1], 0.0, 0.0],
        }
    }

    /// Pack the global white-balance CAT matrix for the GPU (std140 mat3 = 3 × `vec4` columns).
    pub fn to_wb_uniform(&self) -> WbUniform {
        let m = wb_matrix(self.temp, self.tint); // row-major
                                                 // Column j (for `mat3x3 * v` in WGSL) is (m[0][j], m[1][j], m[2][j]).
        WbUniform {
            cols: [
                [m[0][0], m[1][0], m[2][0], 0.0],
                [m[0][1], m[1][1], m[2][1], 0.0],
                [m[0][2], m[1][2], m[2][2], 0.0],
            ],
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

    /// Pack the base tone operator's GPU uniform: the LUT log-exposure domain + hue-protect strength.
    /// `tone_amount` itself is baked into the LUT (CPU-side via `base_curve_lut`), not passed here.
    pub fn to_tone_op(&self) -> ToneOpUniform {
        ToneOpUniform {
            params: [BASE_U_MIN, BASE_U_MAX, BASE_HUE_PROTECT, 0.0],
        }
    }

    /// Build this image's base-curve LUT (`tone_amount` normalized to [0,1]).
    pub fn base_curve_lut(&self) -> Vec<f32> {
        build_base_curve_lut((self.tone_amount / 100.0).clamp(0.0, 1.0))
    }

    /// Pack the crop + straighten geometry for the GPU. `src_aspect` = source W/H; `out_aspect` =
    /// render-target W/H (== src_aspect for preview; == crop pixel aspect for true-dims export, so
    /// the cropped content fills the target with no letterbox).
    pub fn to_geom(&self, src_aspect: f32, out_aspect: f32) -> GeomUniform {
        let c = &self.crop;
        let theta = c.angle.to_radians();
        GeomUniform {
            crop: [c.cx, c.cy, c.hw, c.hh],
            rot: [
                theta.cos(),
                theta.sin(),
                geom_autozoom(c, src_aspect),
                if c.is_identity() { 0.0 } else { 1.0 },
            ],
            aspect: [src_aspect, out_aspect, 0.0, 0.0],
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

/// Detail (sharpen / luma+color NR) + Lens (vignette) scalars + the image texel size, for the GPU.
/// std140-clean: two `vec4` rows (32 bytes). `@binding(9)` in `develop.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ExtraUniform {
    /// (sharpen 0..1.5, nr_luma 0..1, nr_color 0..1, vignette -1..1).
    pub detail: [f32; 4],
    /// (1/width, 1/height, _, _).
    pub texel: [f32; 4],
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

/// Crop + straighten geometry uniform (`@binding(12)`). std140-clean: three `vec4` (48 bytes).
/// `crop` = (cx, cy, hw, hh); `rot` = (cos θ, sin θ, autozoom, active); `aspect` = (src W/H, out W/H,
/// _, _). See `to_geom` / `geom_src_uv` and `geom_resolve` in develop.wgsl.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GeomUniform {
    pub crop: [f32; 4],
    pub rot: [f32; 4],
    pub aspect: [f32; 4],
}

impl Default for GeomUniform {
    fn default() -> Self {
        DevelopParams::default().to_geom(1.0, 1.0)
    }
}

#[cfg(test)]
mod wb_tests {
    use super::*;

    // Apply the (row-major) global WB matrix to a neutral [1,1,1] → per-channel response.
    fn neutral(temp: f32, tint: f32) -> [f32; 3] {
        let m = wb_matrix(temp, tint);
        [
            m[0][0] + m[0][1] + m[0][2],
            m[1][0] + m[1][1] + m[1][2],
            m[2][0] + m[2][1] + m[2][2],
        ]
    }

    #[test]
    fn kim_6500_is_near_d65() {
        let (x, y) = kim_xy(6500.0);
        // Kim's locus at 6500 K is close to but NOT exactly D65 — the WB reference uses this same
        // value so the slider zero is still exact identity (see wb_matrix_zero_is_identity).
        assert!((x - 0.31349).abs() < 1e-3, "x={x}");
        assert!((y - 0.32366).abs() < 1e-3, "y={y}");
    }

    #[test]
    fn wb_matrix_zero_is_identity() {
        let m = wb_matrix(0.0, 0.0);
        let id = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut max = 0.0f32;
        for i in 0..3 {
            for j in 0..3 {
                max = max.max((m[i][j] - id[i][j]).abs());
            }
        }
        assert!(max < 1e-6, "wb_matrix(0,0) must be identity, max dev {max}");
    }

    #[test]
    fn temp_warms_tint_greens() {
        let warm = neutral(80.0, 0.0);
        assert!(warm[0] - warm[2] > 0.05, "temp+ must warm (R>B): {warm:?}");
        let cool = neutral(-80.0, 0.0);
        assert!(cool[0] - cool[2] < -0.05, "temp- must cool (R<B): {cool:?}");
        let magenta = neutral(0.0, 20.0);
        assert!(
            magenta[1] < 1.0,
            "tint+ must reduce G (magenta): {magenta:?}"
        );
        // Every response must be finite.
        for v in warm.iter().chain(cool.iter()).chain(magenta.iter()) {
            assert!(v.is_finite(), "WB response must be finite");
        }
    }
}

#[cfg(test)]
mod base_curve_tests {
    use super::*;

    #[test]
    fn anchors_zero_and_mid_grey_for_all_amounts() {
        for amount in [0.0, 0.5, 1.0] {
            assert_eq!(base_curve_value(0.0, amount), 0.0, "f(0) must be 0");
            let m = base_curve_value(MID_GREY, amount);
            assert!(
                (m - MID_GREY).abs() < 1e-5,
                "mid-grey must anchor at {MID_GREY} for amount {amount} (got {m})"
            );
        }
    }

    #[test]
    fn monotone_bounded_and_compresses_highlights() {
        let xs = [0.001, 0.01, 0.05, 0.18, 0.5, 1.0, 2.0, 4.0, 8.0, 32.0];
        let mut prev = -1.0;
        for x in xs {
            let y = base_curve_value(x, 1.0);
            assert!((0.0..1.0).contains(&y), "f({x})={y} out of [0,1)");
            assert!(y > prev, "f must increase ({prev} -> {y} at x={x})");
            prev = y;
        }
        assert!(base_curve_value(4.0, 1.0) < base_curve_value(8.0, 1.0));
        assert!(base_curve_value(8.0, 1.0) < 1.0);
    }

    #[test]
    fn full_amount_is_more_contrasty_than_neutral() {
        assert!(base_curve_value(1.0, 1.0) > base_curve_value(1.0, 0.0));
    }

    #[test]
    fn lut_matches_curve_at_sample_points() {
        let lut = build_base_curve_lut(1.0);
        assert_eq!(lut.len(), BASE_LUT_SIZE);
        let u_at = |i: usize| {
            BASE_U_MIN + (i as f32 / (BASE_LUT_SIZE - 1) as f32) * (BASE_U_MAX - BASE_U_MIN)
        };
        for i in [0usize, BASE_LUT_SIZE / 2, BASE_LUT_SIZE - 1] {
            let x = MID_GREY * 2f32.powf(u_at(i));
            assert!((lut[i] - base_curve_value(x, 1.0)).abs() < 1e-6);
        }
    }
}

#[cfg(test)]
mod geom_tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2], eps: f32) -> bool {
        (a[0] - b[0]).abs() < eps && (a[1] - b[1]).abs() < eps
    }

    #[test]
    fn identity_crop_maps_to_self() {
        let c = Crop::default();
        assert!(c.is_identity());
        for u in [[0.0, 0.0], [0.5, 0.5], [1.0, 1.0], [0.25, 0.8]] {
            let src = geom_src_uv(&c, 1.5, geom_autozoom(&c, 1.5), u);
            assert!(
                close(src, u, 1e-6),
                "identity must map {u:?} -> self, got {src:?}"
            );
        }
    }

    #[test]
    fn crop_corners_map_to_rect() {
        let c = Crop {
            cx: 0.6,
            cy: 0.4,
            hw: 0.25,
            hh: 0.2,
            angle: 0.0,
        };
        let a = 2.0;
        let z = 1.0;
        assert!(close(geom_src_uv(&c, a, z, [0.0, 0.0]), [0.35, 0.2], 1e-6));
        assert!(close(geom_src_uv(&c, a, z, [1.0, 1.0]), [0.85, 0.6], 1e-6));
        assert!(close(geom_src_uv(&c, a, z, [0.5, 0.5]), [0.6, 0.4], 1e-6));
    }

    #[test]
    fn rotation_is_pixel_space_on_nonsquare() {
        let c = Crop {
            angle: 90.0,
            ..Default::default()
        };
        let src = geom_src_uv(&c, 2.0, 1.0, [0.6, 0.5]);
        assert!(
            close(src, [0.5, 0.3], 1e-5),
            "90° non-square map wrong: {src:?}"
        );
    }

    #[test]
    fn export_rect_is_full_for_identity_and_tight_for_aspect() {
        assert_eq!(Crop::default().export_rect(3000, 2000), (0, 0, 3000, 2000));
        let c = Crop {
            cx: 0.5,
            cy: 0.5,
            hw: 0.5,
            hh: 0.5 / ((16.0 / 9.0) / 1.5),
            angle: 0.0,
        };
        let (_x, y, w, h) = c.export_rect(3000, 2000);
        assert_eq!(w, 3000, "16:9 of 3:2 keeps full width");
        assert!(
            (w as f32 / h as f32 - 16.0 / 9.0).abs() < 0.01,
            "rect must be 16:9"
        );
        assert_eq!(y, (2000 - h) / 2, "crop must be vertically centered");
    }

    #[test]
    fn autozoom_keeps_corners_in_bounds() {
        let c = Crop {
            angle: 45.0,
            ..Default::default()
        };
        let a = 1.0;
        let z = geom_autozoom(&c, a);
        assert!(
            (z - 2.0_f32.sqrt()).abs() < 1e-3,
            "45° square autozoom ≈ √2, got {z}"
        );
        let mut touches = false;
        for u in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]] {
            let s = geom_src_uv(&c, a, z, u);
            assert!(
                s[0] >= -1e-4 && s[0] <= 1.0 + 1e-4 && s[1] >= -1e-4 && s[1] <= 1.0 + 1e-4,
                "corner {u:?} -> {s:?} out of source bounds"
            );
            if s[0].min(1.0 - s[0]).min(s[1]).min(1.0 - s[1]) < 1e-3 {
                touches = true;
            }
        }
        assert!(
            touches,
            "at z_min at least one corner must touch the source edge"
        );
    }
}
