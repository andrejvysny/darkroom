use serde::{Deserialize, Serialize};

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

/// AI raw-domain denoise settings. `enabled` gates whether the backend renders from the denoised
/// linear buffer; `amount` (0..100) blends the denoised result over the original (100 = full). The
/// heavy inference runs once on demand and is cached — `amount` only re-blends two cached buffers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Denoise {
    pub enabled: bool,
    /// Blend amount, 0..100 of the denoised result over the original.
    pub amount: f32,
}

impl Default for Denoise {
    fn default() -> Self {
        Self {
            enabled: false,
            amount: 50.0,
        }
    }
}

impl Denoise {
    /// True when denoise contributes nothing (disabled or zero amount) — the backend then renders
    /// from the clean linear buffer, skipping the denoise decode/inference entirely.
    pub fn is_off(&self) -> bool {
        !self.enabled || self.amount <= 0.0
    }

    /// Blend fraction in 0..1 (`amount/100`, clamped).
    pub fn blend(&self) -> f32 {
        (self.amount / 100.0).clamp(0.0, 1.0)
    }
}

/// Color-balance-RGB grading (a faithful subset of darktable `colorbalancergb`). Each "way" is a
/// per-channel grading-RGB vector applied only over its tonal range (darktable's `opacity_masks`):
/// `global` = offset (all tones), `shadows` = lift, `highlights` = gain, `midtones` = per-channel
/// power. Plus a scene-linear `contrast` (fulcrum 0.18) and global `saturation` (chroma). All fields
/// are 0 by default ⇒ the whole stage is skipped (byte-identical render). Values are normalized to
/// ±1 in the UI; see `to_cbrgb`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct CbRgb {
    /// Global offset (lift applied to every tone), per grading channel, ≈ ±0.5.
    pub global: [f32; 3],
    /// Shadows lift (shadow-masked additive), per grading channel.
    pub shadows: [f32; 3],
    /// Midtones power (midtone-masked per-channel exponent offset).
    pub midtones: [f32; 3],
    /// Highlights gain (highlight-masked multiplicative), per grading channel.
    pub highlights: [f32; 3],
    /// Scene-linear contrast, -1..1 (power about grading-luminance fulcrum 0.18).
    pub contrast: f32,
    /// Global chroma / saturation, -1..1.
    pub saturation: f32,
}

impl Default for CbRgb {
    fn default() -> Self {
        Self {
            global: [0.0; 3],
            shadows: [0.0; 3],
            midtones: [0.0; 3],
            highlights: [0.0; 3],
            contrast: 0.0,
            saturation: 0.0,
        }
    }
}

impl CbRgb {
    /// True when every control is at its no-op default (lets the shader skip the grading-space round
    /// trip entirely → the default render stays byte-identical to a pre-`cb_rgb` build).
    pub fn is_identity(&self) -> bool {
        self.global == [0.0; 3]
            && self.shadows == [0.0; 3]
            && self.midtones == [0.0; 3]
            && self.highlights == [0.0; 3]
            && self.contrast == 0.0
            && self.saturation == 0.0
    }
}

/// Channel mixer (Photoshop/GIMP-style). Each output channel is a weighted sum of the source R/G/B:
/// `out.R = red·[R,G,B]`, etc. Applied on **display-encoded sRGB** (matching Photoshop/GIMP, so a
/// green↔blue swap does what the user expects). Default = identity ⇒ the shader skips the stage
/// (byte-identical render). A pure swap is a permutation of the identity rows. Weights are 1.0 = 100%.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ChannelMix {
    /// Output-red weights over source (R, G, B). Identity = [1, 0, 0].
    pub red: [f32; 3],
    /// Output-green weights over source (R, G, B). Identity = [0, 1, 0].
    pub green: [f32; 3],
    /// Output-blue weights over source (R, G, B). Identity = [0, 0, 1].
    pub blue: [f32; 3],
}

impl Default for ChannelMix {
    fn default() -> Self {
        Self {
            red: [1.0, 0.0, 0.0],
            green: [0.0, 1.0, 0.0],
            blue: [0.0, 0.0, 1.0],
        }
    }
}

impl ChannelMix {
    /// True when the matrix is identity (no-op) ⇒ the shader skips the mix, keeping the default
    /// render byte-identical to a pre-`channel_mix` build.
    pub fn is_identity(&self) -> bool {
        self.red == [1.0, 0.0, 0.0] && self.green == [0.0, 1.0, 0.0] && self.blue == [0.0, 0.0, 1.0]
    }
}

/// Border / frame around the exported (and previewed) image. A solid-color margin sized as a percent
/// of the image's long edge, plus an optional target aspect ratio the image is centered/padded into
/// (e.g. a square 1:1 with a white border). Applied as a CPU post-composite (see `frame_rgba8`), NOT
/// in the GPU shader — so it never touches the develop pipeline, histogram, or thumbnails. No-op by
/// default.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Border {
    /// Border thickness as a percentage of the image's long edge (0 = none).
    pub size: f32,
    /// Border fill color, sRGB 0..1. Default white.
    pub color: [f32; 3],
    /// Target output aspect numerator (0 = keep the photo's aspect).
    pub aspect_w: f32,
    /// Target output aspect denominator (0 = keep the photo's aspect).
    pub aspect_h: f32,
}

impl Default for Border {
    fn default() -> Self {
        Self {
            size: 0.0,
            color: [1.0, 1.0, 1.0],
            aspect_w: 0.0,
            aspect_h: 0.0,
        }
    }
}

impl Border {
    /// True when the border would change the output (a margin, or a target aspect). At the default
    /// (no margin, no aspect) it is inactive and export/preview skip the composite entirely.
    pub fn is_active(&self) -> bool {
        self.size > 0.0 || (self.aspect_w > 0.0 && self.aspect_h > 0.0)
    }

    /// Target aspect as `(w, h)` when both are positive, else `None` (keep the photo's aspect).
    pub fn aspect(&self) -> Option<(f32, f32)> {
        if self.aspect_w > 0.0 && self.aspect_h > 0.0 {
            Some((self.aspect_w, self.aspect_h))
        } else {
            None
        }
    }

    /// Border color as opaque RGBA8 (for the CPU compositor).
    pub fn color_rgba8(&self) -> [u8; 4] {
        let q = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        [q(self.color[0]), q(self.color[1]), q(self.color[2]), 255]
    }
}
