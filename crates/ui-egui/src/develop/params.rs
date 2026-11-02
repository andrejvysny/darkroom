//! One table from a slider to its `ControlSpec` family and the `DevelopParams` field it edits.
//! The UI shows `spec.stored_to_ui(stored)` and writes `spec.ui_to_stored(ui)` (refusing values
//! outside the widget range), so native sliders use exactly the legacy ranges and scaling.

use core_model::controls::{control_spec, ControlSpec};
use core_model::DevelopParams;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HslPart {
    Hue,
    Sat,
    Lum,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    Global,
    Shadows,
    Midtones,
    Highlights,
}

/// A slider's identity. Array-valued fields carry their indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ControlKey {
    Temp,
    Tint,
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    ToneAmount,
    Saturation,
    /// `hsl[band]` component.
    Hsl(u8, HslPart),
    /// `cb_rgb.<zone>[channel]`.
    Cb(Zone, u8),
    CbContrast,
    CbSaturation,
    /// `channel_mix.<output row>[source]`.
    Mix(u8, u8),
    Sharpen,
    NrLuma,
    NrColor,
    Texture,
    Clarity,
    Dehaze,
    Vignette,
    DistK1,
    DistK2,
    CaRed,
    CaBlue,
    CropAngle,
    BorderSize,
}

macro_rules! slot_fn {
    ($name:ident, $($r:tt)+) => {
        fn $name(self, p: $($r)+ DevelopParams) -> $($r)+ f32 {
            match self {
                Self::Temp => $($r)+ p.temp,
                Self::Tint => $($r)+ p.tint,
                Self::Exposure => $($r)+ p.exposure,
                Self::Contrast => $($r)+ p.contrast,
                Self::Highlights => $($r)+ p.highlights,
                Self::Shadows => $($r)+ p.shadows,
                Self::Whites => $($r)+ p.whites,
                Self::Blacks => $($r)+ p.blacks,
                Self::ToneAmount => $($r)+ p.tone_amount,
                Self::Saturation => $($r)+ p.saturation,
                Self::Hsl(band, part) => {
                    let b = $($r)+ p.hsl[usize::from(band)];
                    match part {
                        HslPart::Hue => $($r)+ b.h,
                        HslPart::Sat => $($r)+ b.s,
                        HslPart::Lum => $($r)+ b.l,
                    }
                }
                Self::Cb(zone, c) => {
                    let c = usize::from(c);
                    match zone {
                        Zone::Global => $($r)+ p.cb_rgb.global[c],
                        Zone::Shadows => $($r)+ p.cb_rgb.shadows[c],
                        Zone::Midtones => $($r)+ p.cb_rgb.midtones[c],
                        Zone::Highlights => $($r)+ p.cb_rgb.highlights[c],
                    }
                }
                Self::CbContrast => $($r)+ p.cb_rgb.contrast,
                Self::CbSaturation => $($r)+ p.cb_rgb.saturation,
                Self::Mix(out, src) => {
                    let src = usize::from(src);
                    match out {
                        0 => $($r)+ p.channel_mix.red[src],
                        1 => $($r)+ p.channel_mix.green[src],
                        _ => $($r)+ p.channel_mix.blue[src],
                    }
                }
                Self::Sharpen => $($r)+ p.sharpen,
                Self::NrLuma => $($r)+ p.nr_luma,
                Self::NrColor => $($r)+ p.nr_color,
                Self::Texture => $($r)+ p.texture,
                Self::Clarity => $($r)+ p.clarity,
                Self::Dehaze => $($r)+ p.dehaze,
                Self::Vignette => $($r)+ p.vignette,
                Self::DistK1 => $($r)+ p.dist_k1,
                Self::DistK2 => $($r)+ p.dist_k2,
                Self::CaRed => $($r)+ p.ca_red,
                Self::CaBlue => $($r)+ p.ca_blue,
                Self::CropAngle => $($r)+ p.crop.angle,
                Self::BorderSize => $($r)+ p.border.size,
            }
        }
    };
}

impl ControlKey {
    slot_fn!(slot, &);
    slot_fn!(slot_mut, &mut);

    fn spec_id(self) -> &'static str {
        match self {
            Self::Temp => "temp",
            Self::Tint => "tint",
            Self::Exposure => "exposure",
            Self::Contrast => "contrast",
            Self::Highlights => "highlights",
            Self::Shadows => "shadows",
            Self::Whites => "whites",
            Self::Blacks => "blacks",
            Self::ToneAmount => "toneAmount",
            Self::Saturation => "saturation",
            Self::Hsl(_, HslPart::Hue) => "hsl.h",
            Self::Hsl(_, HslPart::Sat) => "hsl.s",
            Self::Hsl(_, HslPart::Lum) => "hsl.l",
            Self::Cb(..) => "cbRgb.channel",
            Self::CbContrast => "cbRgb.contrast",
            Self::CbSaturation => "cbRgb.saturation",
            Self::Mix(out, src) if out == src => "channelMix.diagonal",
            Self::Mix(..) => "channelMix.offDiagonal",
            Self::Sharpen => "sharpen",
            Self::NrLuma => "nrLuma",
            Self::NrColor => "nrColor",
            Self::Texture => "texture",
            Self::Clarity => "clarity",
            Self::Dehaze => "dehaze",
            Self::Vignette => "vignette",
            Self::DistK1 => "distK1",
            Self::DistK2 => "distK2",
            Self::CaRed => "caRed",
            Self::CaBlue => "caBlue",
            Self::CropAngle => "crop.angle",
            Self::BorderSize => "border.size",
        }
    }

    pub fn spec(self) -> &'static ControlSpec {
        control_spec(self.spec_id()).expect("every ControlKey maps to a CONTROL_SPECS entry")
    }

    /// Stable string for test/automation lookups (`"contrast"`, `"hsl.2.h"`, `"mix.0.1"`).
    pub fn name(self) -> String {
        match self {
            Self::Hsl(band, part) => {
                let p = match part {
                    HslPart::Hue => "h",
                    HslPart::Sat => "s",
                    HslPart::Lum => "l",
                };
                format!("hsl.{band}.{p}")
            }
            Self::Cb(zone, c) => format!("cb.{zone:?}.{c}").to_lowercase(),
            Self::Mix(out, src) => format!("mix.{out}.{src}"),
            other => other.spec_id().to_owned(),
        }
    }

    /// Stored value as the widget shows it (`spec.stored_to_ui`); a non-finite stored value
    /// falls back to the reset position rather than a NaN knob.
    pub fn display(self, params: &DevelopParams) -> f64 {
        let stored = f64::from(*self.slot(params));
        self.spec()
            .stored_to_ui(stored)
            .unwrap_or_else(|| self.reset_ui())
    }

    /// `params` with this control set to the widget value `ui`; `None` when `ui` is outside the
    /// widget range (the spec refuses it).
    pub fn with_ui(self, params: &DevelopParams, ui: f64) -> Option<DevelopParams> {
        let stored = self.spec().ui_to_stored(ui)?;
        let mut next = params.clone();
        *self.slot_mut(&mut next) = stored as f32;
        Some(next)
    }

    /// Widget value a reset lands on. Identical to `spec.reset_value()` except the channel-mixer
    /// diagonal, whose identity is 100 % although the spec is bipolar (reset 0 would zero a channel).
    pub fn reset_ui(self) -> f64 {
        let spec = self.spec();
        if matches!(self, Self::Mix(out, src) if out == src) {
            spec.default
        } else {
            spec.reset_value()
        }
    }

    /// Puts this control at the value a fresh `DevelopParams` has.
    pub fn reset(self, params: &mut DevelopParams) {
        let fresh = DevelopParams::default();
        *self.slot_mut(params) = *self.slot(&fresh);
    }
}

#[cfg(test)]
pub(crate) fn all_keys() -> Vec<ControlKey> {
    let mut keys = vec![
        ControlKey::Temp,
        ControlKey::Tint,
        ControlKey::Exposure,
        ControlKey::Contrast,
        ControlKey::Highlights,
        ControlKey::Shadows,
        ControlKey::Whites,
        ControlKey::Blacks,
        ControlKey::ToneAmount,
        ControlKey::Saturation,
        ControlKey::CbContrast,
        ControlKey::CbSaturation,
        ControlKey::Sharpen,
        ControlKey::NrLuma,
        ControlKey::NrColor,
        ControlKey::Texture,
        ControlKey::Clarity,
        ControlKey::Dehaze,
        ControlKey::Vignette,
        ControlKey::DistK1,
        ControlKey::DistK2,
        ControlKey::CaRed,
        ControlKey::CaBlue,
        ControlKey::CropAngle,
        ControlKey::BorderSize,
    ];
    for band in 0..8 {
        for part in [HslPart::Hue, HslPart::Sat, HslPart::Lum] {
            keys.push(ControlKey::Hsl(band, part));
        }
    }
    for zone in [
        Zone::Global,
        Zone::Shadows,
        Zone::Midtones,
        Zone::Highlights,
    ] {
        for c in 0..3 {
            keys.push(ControlKey::Cb(zone, c));
        }
    }
    for out in 0..3 {
        for src in 0..3 {
            keys.push(ControlKey::Mix(out, src));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_round_trips_through_set_and_get() {
        for key in all_keys() {
            let spec = key.spec();
            for fraction in [0.0, 0.25, 0.8, 1.0] {
                let ui = spec.min + (spec.max - spec.min) * fraction;
                let next = key
                    .with_ui(&DevelopParams::default(), ui)
                    .unwrap_or_else(|| panic!("{key:?} refused {ui}"));
                let shown = key.display(&next);
                assert!(
                    (shown - ui).abs() < 1e-3 * (spec.max - spec.min),
                    "{key:?}: wrote {ui}, displays {shown}"
                );
            }
        }
    }

    #[test]
    fn keys_write_distinct_fields() {
        let base = DevelopParams::default();
        for key in all_keys() {
            let spec = key.spec();
            let changed = key.with_ui(&base, spec.max).unwrap();
            let others: Vec<ControlKey> = all_keys()
                .into_iter()
                .filter(|k| *k != key && k.display(&changed) != k.display(&base))
                .collect();
            assert!(others.is_empty(), "{key:?} also moved {others:?}");
        }
    }

    #[test]
    fn out_of_range_and_non_finite_writes_are_refused() {
        let base = DevelopParams::default();
        for key in all_keys() {
            let spec = key.spec();
            assert!(key.with_ui(&base, spec.max + 1.0).is_none());
            assert!(key.with_ui(&base, spec.min - 1.0).is_none());
            assert!(key.with_ui(&base, f64::NAN).is_none());
        }
    }

    #[test]
    fn defaults_display_the_reset_value() {
        let fresh = DevelopParams::default();
        for key in all_keys() {
            assert!(
                (key.display(&fresh) - key.reset_ui()).abs() < 1e-6,
                "{key:?}: {} vs {}",
                key.display(&fresh),
                key.reset_ui()
            );
            if !matches!(key, ControlKey::Mix(a, b) if a == b) {
                assert_eq!(key.reset_ui(), key.spec().reset_value(), "{key:?}");
            }
        }
    }

    #[test]
    fn reset_restores_the_fresh_value() {
        for key in all_keys() {
            let spec = key.spec();
            let mut p = key.with_ui(&DevelopParams::default(), spec.max).unwrap();
            key.reset(&mut p);
            assert_eq!(p, DevelopParams::default(), "{key:?}");
        }
    }

    #[test]
    fn scaled_families_use_the_spec_conversion() {
        let p = ControlKey::Cb(Zone::Shadows, 1)
            .with_ui(&DevelopParams::default(), 100.0)
            .unwrap();
        assert_eq!(p.cb_rgb.shadows[1], 0.5);
        let p = ControlKey::Mix(0, 0)
            .with_ui(&DevelopParams::default(), 150.0)
            .unwrap();
        assert_eq!(p.channel_mix.red[0], 1.5);
        let p = ControlKey::CbContrast
            .with_ui(&DevelopParams::default(), -50.0)
            .unwrap();
        assert_eq!(p.cb_rgb.contrast, -0.5);
    }
}
