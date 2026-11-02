//! Numeric widget specifications derived from the existing Develop UI.
//! Ranges describe widget input, not validity of previously saved creative edits.
use crate::ui_numeric::js_round;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlScope {
    PersistedEdit,
    TransientWidget,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ControlSpec {
    pub id: &'static str,
    pub scope: ControlScope,
    pub min: f64,
    pub max: f64,
    /// Slider's declared default; bipolar double-click resets to zero instead.
    pub default: f64,
    /// None means continuous pointer input; displayed precision does not quantize it.
    pub step: Option<f64>,
    pub decimals: u8,
    pub bipolar: bool,
    pub suffix: &'static str,
    /// Stored value = UI value * scale. Readback may additionally round for display.
    pub scale: f64,
    pub rounded_readback: bool,
}

impl ControlSpec {
    pub fn accepts_ui_value(&self, value: f64) -> bool {
        value.is_finite() && value >= self.min && value <= self.max
    }

    /// Converts a widget edit only when it is finite and within the widget range.
    pub fn ui_to_stored(&self, value: f64) -> Option<f64> {
        if !self.accepts_ui_value(value) {
            return None;
        }
        Some(if self.id == "cbRgb.channel" {
            (value / 100.0) * 0.5
        } else if self.scale == 0.01 {
            value / 100.0
        } else {
            value
        })
    }

    /// Displays finite stored values without clamping or altering the saved edit.
    pub fn stored_to_ui(&self, value: f64) -> Option<f64> {
        let ui = if self.id == "cbRgb.channel" {
            (value / 0.5) * 100.0
        } else if self.scale == 0.01 {
            value * 100.0
        } else {
            value
        };
        if !ui.is_finite() {
            return None;
        }
        Some(if self.rounded_readback {
            js_round(ui)
        } else {
            ui
        })
    }

    pub fn reset_value(&self) -> f64 {
        if self.bipolar {
            0.0
        } else {
            self.default
        }
    }
}

#[allow(clippy::too_many_arguments)]
const fn slider(
    id: &'static str,
    min: f64,
    max: f64,
    default: f64,
    decimals: u8,
    bipolar: bool,
    scale: f64,
    rounded_readback: bool,
    scope: ControlScope,
) -> ControlSpec {
    ControlSpec {
        id,
        scope,
        min,
        max,
        default,
        step: None,
        decimals,
        bipolar,
        suffix: "",
        scale,
        rounded_readback,
    }
}

#[allow(clippy::too_many_arguments)]
const fn edit(
    id: &'static str,
    min: f64,
    max: f64,
    default: f64,
    decimals: u8,
    bipolar: bool,
    scale: f64,
    rounded: bool,
) -> ControlSpec {
    slider(
        id,
        min,
        max,
        default,
        decimals,
        bipolar,
        scale,
        rounded,
        ControlScope::PersistedEdit,
    )
}

const fn bipolar(id: &'static str) -> ControlSpec {
    edit(id, -100.0, 100.0, 0.0, 0, true, 1.0, false)
}

// InstrumentPanel.tsx, ColorMixer.tsx, ColorBalance.tsx, ChannelMixer.tsx,
// Border.tsx, Denoise.tsx and MaskPanel.tsx; pointer and reset behavior: Slider.tsx.
pub const CONTROL_SPECS: &[ControlSpec] = &[
    bipolar("temp"),
    bipolar("tint"),
    edit("exposure", -5.0, 5.0, 0.0, 2, true, 1.0, false),
    bipolar("contrast"),
    bipolar("highlights"),
    bipolar("shadows"),
    bipolar("whites"),
    bipolar("blacks"),
    edit("toneAmount", 0.0, 100.0, 100.0, 0, false, 1.0, false),
    bipolar("saturation"),
    bipolar("hsl.h"),
    bipolar("hsl.s"),
    bipolar("hsl.l"),
    edit("cbRgb.channel", -100.0, 100.0, 0.0, 0, true, 0.005, false),
    edit("cbRgb.contrast", -100.0, 100.0, 0.0, 0, true, 0.01, false),
    edit("cbRgb.saturation", -100.0, 100.0, 0.0, 0, true, 0.01, false),
    edit(
        "channelMix.diagonal",
        -200.0,
        200.0,
        100.0,
        0,
        true,
        0.01,
        false,
    ),
    edit(
        "channelMix.offDiagonal",
        -200.0,
        200.0,
        0.0,
        0,
        true,
        0.01,
        false,
    ),
    edit("sharpen", 0.0, 150.0, 0.0, 0, false, 1.0, false),
    edit("nrLuma", 0.0, 100.0, 0.0, 0, false, 1.0, false),
    edit("nrColor", 0.0, 100.0, 0.0, 0, false, 1.0, false),
    edit("denoise.amount", 0.0, 100.0, 50.0, 0, false, 1.0, false),
    bipolar("texture"),
    bipolar("clarity"),
    bipolar("dehaze"),
    bipolar("vignette"),
    bipolar("distK1"),
    bipolar("distK2"),
    bipolar("caRed"),
    bipolar("caBlue"),
    edit("crop.angle", -45.0, 45.0, 0.0, 1, true, 1.0, false),
    edit("border.size", 0.0, 25.0, 0.0, 1, false, 1.0, false),
    edit(
        "mask.luminanceRange.lo",
        0.0,
        100.0,
        40.0,
        0,
        false,
        0.01,
        true,
    ),
    edit(
        "mask.luminanceRange.hi",
        0.0,
        100.0,
        100.0,
        0,
        false,
        0.01,
        true,
    ),
    edit(
        "mask.luminanceRange.feather",
        0.0,
        50.0,
        8.0,
        0,
        false,
        0.01,
        true,
    ),
    edit("mask.colorRange.tol", 1.0, 50.0, 8.0, 0, false, 0.01, true),
    edit(
        "mask.colorRange.feather",
        0.0,
        50.0,
        6.0,
        0,
        false,
        0.01,
        true,
    ),
    edit("mask.opacity", 0.0, 100.0, 100.0, 0, false, 0.01, true),
    edit("mask.adjust.exposure", -5.0, 5.0, 0.0, 2, true, 1.0, false),
    bipolar("mask.adjust.contrast"),
    bipolar("mask.adjust.highlights"),
    bipolar("mask.adjust.shadows"),
    bipolar("mask.adjust.whites"),
    bipolar("mask.adjust.blacks"),
    bipolar("mask.adjust.saturation"),
    bipolar("mask.adjust.temp"),
    bipolar("mask.adjust.tint"),
    // PresetsPanel.tsx uses a native range input; its default step is 1.
    ControlSpec {
        id: "preset.amount",
        scope: ControlScope::TransientWidget,
        min: 0.0,
        max: 100.0,
        default: 100.0,
        step: Some(1.0),
        decimals: 0,
        bipolar: false,
        suffix: "",
        scale: 1.0,
        rounded_readback: false,
    },
    slider(
        "brush.size",
        1.0,
        50.0,
        8.0,
        0,
        false,
        0.01,
        true,
        ControlScope::TransientWidget,
    ),
    slider(
        "brush.hardness",
        0.0,
        100.0,
        50.0,
        0,
        false,
        0.01,
        true,
        ControlScope::TransientWidget,
    ),
    slider(
        "brush.opacity",
        0.0,
        100.0,
        100.0,
        0,
        false,
        0.01,
        true,
        ControlScope::TransientWidget,
    ),
];

pub fn control_spec(id: &str) -> Option<&'static ControlSpec> {
    CONTROL_SPECS.iter().find(|spec| spec.id == id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HslComponent {
    Hue,
    Saturation,
    Luminance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorZone {
    Global,
    Shadows,
    Midtones,
    Highlights,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RgbChannel {
    Red,
    Green,
    Blue,
}

impl RgbChannel {
    fn index(self) -> u8 {
        match self {
            Self::Red => 0,
            Self::Green => 1,
            Self::Blue => 2,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Green => "green",
            Self::Blue => "blue",
        }
    }
}

/// Concrete array fields use indices matching the serialized edit model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldControl {
    Hsl {
        band: u8,
        component: HslComponent,
    },
    ColorBalance {
        zone: ColorZone,
        channel: RgbChannel,
    },
    ChannelMix {
        output: RgbChannel,
        source: RgbChannel,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedControl {
    pub field_id: String,
    /// Family specification; its `id` is the family name, not `field_id`.
    pub spec: &'static ControlSpec,
}

impl FieldControl {
    pub fn resolve(self) -> Option<ResolvedControl> {
        let (field_id, family) = match self {
            Self::Hsl { band, component } if band < 8 => {
                let name = match component {
                    HslComponent::Hue => "h",
                    HslComponent::Saturation => "s",
                    HslComponent::Luminance => "l",
                };
                (format!("hsl.{band}.{name}"), format!("hsl.{name}"))
            }
            Self::Hsl { .. } => return None,
            Self::ColorBalance { zone, channel } => {
                let zone = match zone {
                    ColorZone::Global => "global",
                    ColorZone::Shadows => "shadows",
                    ColorZone::Midtones => "midtones",
                    ColorZone::Highlights => "highlights",
                };
                (
                    format!("cbRgb.{zone}.{}", channel.index()),
                    "cbRgb.channel".to_owned(),
                )
            }
            Self::ChannelMix { output, source } => {
                let family = if output == source {
                    "channelMix.diagonal"
                } else {
                    "channelMix.offDiagonal"
                };
                (
                    format!("channelMix.{}.{}", output.name(), source.index()),
                    family.to_owned(),
                )
            }
        };
        Some(ResolvedControl {
            field_id,
            spec: control_spec(&family)?,
        })
    }
}
