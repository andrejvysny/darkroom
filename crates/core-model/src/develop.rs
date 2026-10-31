use serde::{Deserialize, Serialize};

use crate::{Border, CbRgb, ChannelMix, Crop, Denoise, HslBand, Mask, ToneCurve, HSL_BANDS};

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
    /// Manual lens distortion, primary radial coefficient (k1), -100..100. Positive = pincushion
    /// correction of barrel (pulls corners in); negative = barrel. 0 = off.
    pub dist_k1: f32,
    /// Manual lens distortion, secondary radial coefficient (k2), -100..100. Fine higher-order term.
    pub dist_k2: f32,
    /// Lateral chromatic-aberration correction, red/cyan radial scale, -100..100. 0 = off.
    pub ca_red: f32,
    /// Lateral chromatic-aberration correction, blue/yellow radial scale, -100..100. 0 = off.
    pub ca_blue: f32,
    /// Clarity (mid-tone local contrast), -100..100. 0 = off.
    pub clarity: f32,
    /// Texture (fine local contrast), -100..100. 0 = off.
    pub texture: f32,
    /// Dehaze (coarse local contrast + black-point pull), -100..100. 0 = off.
    pub dehaze: f32,
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
    /// Local adjustment masks, in stacking order. Within each processing domain, each mask applies
    /// its local controls to the current pixel and blends before/after by coverage × opacity.
    /// Global controls run once, followed by scene-linear mask controls, the display transition,
    /// then display mask controls. Neutral/disabled masks are no-ops; non-neutral masks may be
    /// order-dependent. Empty by default (v1 edits deserialize here).
    #[serde(default)]
    pub masks: Vec<Mask>,
    /// Color-balance-RGB grading (4-way + scene-linear contrast/saturation). No-op by default.
    #[serde(default)]
    pub cb_rgb: CbRgb,
    /// Channel mixer (Photoshop/GIMP-style 3×3 remix on display sRGB). Identity by default; a pure
    /// channel swap (e.g. green↔blue) is a permutation of the identity rows.
    #[serde(default)]
    pub channel_mix: ChannelMix,
    /// Border / frame around the image (solid color + optional pad-to-aspect). A post-composite the
    /// GPU shader never sees — applied only by export + the develop-stage preview. No-op by default.
    #[serde(default)]
    pub border: Border,
    /// AI raw-domain denoise (Lightroom "Denoise"). Applied on-demand to the raw mosaic BEFORE the
    /// develop pipeline — NOT a GPU uniform; it selects which linear buffer (clean vs denoised) the
    /// backend feeds to `prepare()`, and `amount` blends between them. No-op (disabled) by default.
    #[serde(default)]
    pub denoise: Denoise,
    /// `true` when the source is an already-developed **display-referred** image (JPEG/PNG): the
    /// scene-referred ACR base tone operator is bypassed so an unedited image round-trips to itself.
    /// Intrinsic to the image and derived by the backend from the file format at render time — it is
    /// NOT a user edit, so it is `#[serde(skip)]` (never persisted, transferred, or preset-able; it
    /// stays out of the serialized edit, keeping every RAW render and the preset scope unchanged).
    #[serde(skip)]
    pub display_referred: bool,
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
            dist_k1: 0.0,
            dist_k2: 0.0,
            ca_red: 0.0,
            ca_blue: 0.0,
            clarity: 0.0,
            texture: 0.0,
            dehaze: 0.0,
            tone_amount: 100.0,
            tone_curve: ToneCurve::default(),
            hsl: [HslBand::default(); HSL_BANDS],
            crop: Crop::default(),
            masks: Vec::new(),
            cb_rgb: CbRgb::default(),
            channel_mix: ChannelMix::default(),
            border: Border::default(),
            denoise: Denoise::default(),
            display_referred: false,
        }
    }
}

impl DevelopParams {
    /// True when any Presence (clarity/texture/dehaze) control is engaged.
    pub fn local_is_active(&self) -> bool {
        self.clarity != 0.0 || self.texture != 0.0 || self.dehaze != 0.0
    }

    /// True when the HSL mixer is a no-op (lets the shader skip the RGB↔HSV round-trip cheaply
    /// via the uniform, but kept here for clarity/tests).
    pub fn hsl_is_identity(&self) -> bool {
        self.hsl
            .iter()
            .all(|b| b.h == 0.0 && b.s == 0.0 && b.l == 0.0)
    }
}
