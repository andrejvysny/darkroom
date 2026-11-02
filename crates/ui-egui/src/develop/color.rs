//! Colour modules: Color mixer (global saturation + 8 hue bands), Color balance (four tonal
//! zones + master) and Channel mixer, mirroring `ColorMixer.tsx`, `ColorBalance.tsx` and
//! `ChannelMixer.tsx`. All sliders go through the generic develop slider.

use super::params::{ControlKey as K, HslPart, Zone};
use super::slider::{develop_slider, Env};
use super::DevelopUi;
use crate::theme;
use crate::widgets::{describe_selected, focus_ring, pill_button};
use core_model::DevelopParams;
use egui::{Color32, RichText, Sense, Stroke, Ui};

const BAND_NAMES: [&str; 8] = [
    "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
];

/// Band swatches, in `develop.wgsl` `HUE_CENTERS` order: red, orange, yellow, green, aqua, blue,
/// purple, magenta.
const BANDS: [Color32; 8] = [
    Color32::from_rgb(0xc8, 0x53, 0x4f),
    Color32::from_rgb(0xcd, 0x8b, 0x4a),
    Color32::from_rgb(0xcd, 0xc0, 0x4a),
    Color32::from_rgb(0x7b, 0xb2, 0x4f),
    Color32::from_rgb(0x4f, 0xb3, 0x9a),
    Color32::from_rgb(0x4f, 0x86, 0xc8),
    Color32::from_rgb(0x7d, 0x5f, 0xc0),
    Color32::from_rgb(0xc2, 0x5f, 0xa6),
];

fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .size(11.0)
            .color(theme::T2)
            .extra_letter_spacing(0.5),
    );
}

pub(super) fn mixer(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    develop_slider(ui, "Saturation", K::Saturation, env, st);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        for (i, color) in BANDS.iter().enumerate() {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), Sense::click());
            st.band_dots.push(rect);
            describe_selected(
                &response,
                egui::accesskit::Role::RadioButton,
                env.enabled,
                usize::from(st.band) == i,
                &format!("{} band", BAND_NAMES[i]),
            );
            focus_ring(ui, &response, rect, 11);
            let painter = ui.painter();
            painter.circle_filled(rect.center(), 9.0, *color);
            if usize::from(st.band) == i {
                painter.circle_stroke(rect.center(), 10.0, Stroke::new(2.0, theme::T1));
            } else if response.hovered() {
                painter.circle_stroke(rect.center(), 10.0, Stroke::new(1.0, theme::LINE2));
            }
            if response.clicked() {
                st.band = i as u8;
            }
        }
    });
    let band = st.band.min(7);
    for (label, part) in [
        ("Hue", HslPart::Hue),
        ("Saturation", HslPart::Sat),
        ("Luminance", HslPart::Lum),
    ] {
        develop_slider(ui, label, K::Hsl(band, part), env, st);
    }
}

pub(super) fn balance(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    for (title, zone) in [
        ("Global", Zone::Global),
        ("Shadows", Zone::Shadows),
        ("Midtones", Zone::Midtones),
        ("Highlights", Zone::Highlights),
    ] {
        heading(ui, title);
        for (c, label) in ["Red", "Green", "Blue"].into_iter().enumerate() {
            develop_slider(ui, label, K::Cb(zone, c as u8), env, st);
        }
    }
    heading(ui, "Master");
    develop_slider(ui, "Contrast", K::CbContrast, env, st);
    develop_slider(ui, "Saturation", K::CbSaturation, env, st);
}

fn swap_rows(p: &mut DevelopParams, a: usize, b: usize) {
    let m = &mut p.channel_mix;
    let mut rows = [m.red, m.green, m.blue];
    rows.swap(a, b);
    [m.red, m.green, m.blue] = rows;
}

pub(super) fn channel_mixer(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (label, a, b) in [
            ("Swap G / B", 1, 2),
            ("Swap R / G", 0, 1),
            ("Swap R / B", 0, 2),
        ] {
            if pill_button(ui, None, label, env.enabled).clicked() {
                env.edit(|p| swap_rows(p, a, b));
            }
        }
    });
    for (out, title) in ["Red output", "Green output", "Blue output"]
        .into_iter()
        .enumerate()
    {
        heading(ui, title);
        for (src, label) in ["R", "G", "B"].into_iter().enumerate() {
            develop_slider(ui, label, K::Mix(out as u8, src as u8), env, st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::swap_rows;
    use core_model::DevelopParams;

    #[test]
    fn swapping_green_and_blue_rows_is_a_permutation_of_identity() {
        let mut p = DevelopParams::default();
        swap_rows(&mut p, 1, 2);
        assert_eq!(p.channel_mix.green, [0.0, 0.0, 1.0]);
        assert_eq!(p.channel_mix.blue, [0.0, 1.0, 0.0]);
        assert_eq!(p.channel_mix.red, [1.0, 0.0, 0.0]);
    }
}
