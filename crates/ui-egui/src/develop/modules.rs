//! The instrument panel's collapsible modules, in the legacy `InstrumentPanel.tsx` order with the
//! same titles, slider labels and reset semantics. Anything the native app cannot do yet is drawn
//! disabled with the "Not in the native app yet" hover — never a control that silently does nothing.

use super::params::ControlKey as K;
use super::slider::{develop_slider, disabled_slider, Env};
use super::{border, color, crop_tool, curve, DevelopUi};
use crate::icons::{icon_button_enabled, Icon};
use crate::widgets::{pill_button, pill_toggle};
use crate::{theme, NOT_YET};
use core_model::{Border, CbRgb, ChannelMix, Crop, DevelopParams, HslBand, ToneCurve, HSL_BANDS};
use egui::{Align2, FontId, Id, Margin, Pos2, Rect, Sense, Stroke, Ui};

const HEADER_H: f32 = 34.0;
const SIDE: f32 = 14.0;
const DENOISE_NOTE: &str = "AI denoise isn't rendered by the native app yet";

type Reset = fn(&mut DevelopParams);

pub(super) fn all(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    masks(ui, env, st);
    module(
        ui,
        env,
        st,
        Head::new("wb", "White balance", true, Some(reset_wb)),
        white_balance,
    );
    module(
        ui,
        env,
        st,
        Head::new("light", "Light", true, Some(reset_light)),
        light,
    );
    module(
        ui,
        env,
        st,
        Head::new("curve", "Tone curve", true, Some(reset_curve)),
        |ui, env, st| {
            develop_slider(ui, "Base curve", K::ToneAmount, env, st);
            curve::tone_curve(ui, env, st);
        },
    );
    module(
        ui,
        env,
        st,
        Head::new("mixer", "Color mixer", true, Some(reset_mixer)),
        color::mixer,
    );
    module(
        ui,
        env,
        st,
        Head::new("balance", "Color balance", false, Some(reset_balance)),
        color::balance,
    );
    module(
        ui,
        env,
        st,
        Head::new("chanmix", "Channel mixer", false, Some(reset_chanmix)),
        color::channel_mixer,
    );
    module(
        ui,
        env,
        st,
        Head::new("detail", "Detail", false, Some(reset_detail)),
        detail,
    );
    module(
        ui,
        env,
        st,
        Head::new("denoise", "Denoise", false, Some(reset_denoise)),
        denoise,
    );
    module(
        ui,
        env,
        st,
        Head::new("presence", "Presence", false, Some(reset_presence)),
        presence,
    );
    module(
        ui,
        env,
        st,
        Head::new("lens", "Lens corrections", false, Some(reset_lens)),
        lens,
    );
    module(
        ui,
        env,
        st,
        Head::new("crop", "Crop & straighten", false, Some(reset_crop)),
        crop,
    );
    module(
        ui,
        env,
        st,
        Head::new("border", "Border", false, Some(reset_border)),
        border::panel,
    );
}

fn reset_keys(p: &mut DevelopParams, keys: &[K]) {
    for key in keys {
        key.reset(p);
    }
}

fn reset_wb(p: &mut DevelopParams) {
    reset_keys(p, &[K::Temp, K::Tint]);
}
fn reset_light(p: &mut DevelopParams) {
    reset_keys(
        p,
        &[
            K::Exposure,
            K::Contrast,
            K::Highlights,
            K::Shadows,
            K::Blacks,
            K::Whites,
        ],
    );
}
fn reset_curve(p: &mut DevelopParams) {
    K::ToneAmount.reset(p);
    p.tone_curve = ToneCurve::default();
}
fn reset_mixer(p: &mut DevelopParams) {
    K::Saturation.reset(p);
    p.hsl = [HslBand::default(); HSL_BANDS];
}
fn reset_balance(p: &mut DevelopParams) {
    p.cb_rgb = CbRgb::default();
}
fn reset_chanmix(p: &mut DevelopParams) {
    p.channel_mix = ChannelMix::default();
}
fn reset_detail(p: &mut DevelopParams) {
    reset_keys(p, &[K::Sharpen, K::NrLuma, K::NrColor]);
}
fn reset_denoise(p: &mut DevelopParams) {
    p.denoise.enabled = false;
}
fn reset_presence(p: &mut DevelopParams) {
    reset_keys(p, &[K::Clarity, K::Texture, K::Dehaze]);
}
fn reset_lens(p: &mut DevelopParams) {
    reset_keys(p, &[K::Vignette, K::DistK1, K::DistK2, K::CaRed, K::CaBlue]);
}
fn reset_crop(p: &mut DevelopParams) {
    p.crop = Crop::default();
}
fn reset_border(p: &mut DevelopParams) {
    p.border = Border::default();
}

/// A module header's static description.
struct Head {
    id: &'static str,
    title: &'static str,
    open_by_default: bool,
    reset: Option<Reset>,
}

impl Head {
    fn new(
        id: &'static str,
        title: &'static str,
        open_by_default: bool,
        reset: Option<Reset>,
    ) -> Self {
        Self {
            id,
            title,
            open_by_default,
            reset,
        }
    }
}

/// Header (chevron, title, "Reset") plus the body while the module is open.
fn module(
    ui: &mut Ui,
    env: &mut Env<'_>,
    st: &mut DevelopUi,
    head: Head,
    body: impl FnOnce(&mut Ui, &mut Env<'_>, &mut DevelopUi),
) {
    let Head {
        id,
        title,
        open_by_default,
        reset,
    } = head;
    let open = *st.open.entry(id).or_insert(open_by_default);
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), HEADER_H), Sense::hover());
    let reset_rect = Rect::from_min_size(
        Pos2::new(rect.right() - SIDE - 34.0, rect.top()),
        egui::vec2(34.0 + SIDE, HEADER_H),
    );
    let head_rect = if reset.is_some() {
        Rect::from_min_max(rect.min, Pos2::new(reset_rect.left(), rect.bottom()))
    } else {
        rect
    };
    let header = ui.interact(
        head_rect,
        Id::unique(("develop_module", id)),
        Sense::click(),
    );
    header.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, true, title));
    ui.ctx().accesskit_node_builder(header.id, |node| {
        node.set_expanded(open);
    });
    crate::widgets::focus_ring(ui, &header, head_rect, 0);
    if header.hovered() {
        ui.painter()
            .rect_filled(rect, 0, theme::HOVER.gamma_multiply(0.25));
    }
    let chevron = if open {
        Icon::ChevronDown
    } else {
        Icon::ChevronRight
    };
    crate::icons::paint(
        ui.painter(),
        Rect::from_center_size(
            Pos2::new(rect.left() + SIDE + 6.0, rect.center().y),
            egui::vec2(12.0, 12.0),
        ),
        chevron,
        theme::T3,
    );
    ui.painter().text(
        Pos2::new(rect.left() + SIDE + 22.0, rect.center().y),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(13.0),
        theme::T1,
    );
    if header.clicked() {
        st.open.insert(id, !open);
    }
    if let Some(reset) = reset {
        let sense = if env.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let r = ui.interact(reset_rect, Id::unique(("develop_module_reset", id)), sense);
        st.module_resets.insert(id, reset_rect);
        r.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::accesskit::Role::Button,
                env.enabled,
                format!("Reset {title}"),
            )
        });
        crate::widgets::focus_ring(ui, &r, reset_rect, 2);
        let color = if env.enabled && r.hovered() {
            theme::T1
        } else {
            theme::T3
        };
        ui.painter().text(
            Pos2::new(rect.right() - SIDE, rect.center().y),
            Align2::RIGHT_CENTER,
            "Reset",
            FontId::proportional(10.5),
            color,
        );
        if r.clicked() {
            env.edit(reset);
        }
    }
    if open {
        egui::Frame::new()
            .inner_margin(Margin {
                left: SIDE as i8,
                right: SIDE as i8,
                top: 2,
                bottom: 14,
            })
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 13.0;
                body(ui, env, st);
            });
    }
    let y = ui.cursor().top();
    let x = ui.max_rect().x_range();
    ui.painter().hline(x, y, Stroke::new(1.0, theme::LINE));
}

fn masks(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    module(
        ui,
        env,
        st,
        Head::new("masks", "Masks", false, None),
        |ui, env, _| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for label in [
                    "+ Linear",
                    "+ Radial",
                    "+ Brush",
                    "+ Luma",
                    "+ Color",
                    "+ AI Select",
                ] {
                    pill_button(ui, None, label, false).on_hover_text(NOT_YET);
                }
            });
            let count = env.params.map_or(0, |p| p.masks.len());
            let hint = if count == 0 {
                "Add a mask to make a local adjustment.".to_owned()
            } else {
                format!("{count} mask(s) in this edit can't be edited in the native app yet.")
            };
            ui.label(egui::RichText::new(hint).size(11.5).color(theme::T3));
        },
    );
}

fn white_balance(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("As shot").size(12.0).color(theme::T3))
            .on_hover_text(NOT_YET);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            icon_button_enabled(ui, Icon::Eyedropper, NOT_YET, false);
            pill_button(ui, None, "Custom", false).on_hover_text(NOT_YET);
        });
    });
    develop_slider(ui, "Temp", K::Temp, env, st);
    develop_slider(ui, "Tint", K::Tint, env, st);
}

fn light(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    for (label, key) in [
        ("Exposure", K::Exposure),
        ("Contrast", K::Contrast),
        ("Highlights", K::Highlights),
        ("Shadows", K::Shadows),
        ("Whites", K::Whites),
        ("Blacks", K::Blacks),
    ] {
        develop_slider(ui, label, key, env, st);
    }
}

fn detail(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    develop_slider(ui, "Sharpening", K::Sharpen, env, st);
    develop_slider(ui, "Noise · luminance", K::NrLuma, env, st);
    develop_slider(ui, "Noise · color", K::NrColor, env, st);
}

fn denoise(ui: &mut Ui, env: &mut Env<'_>, _st: &mut DevelopUi) {
    let enabled = env.params.is_some_and(|p| p.denoise.enabled);
    if enabled {
        ui.label(
            egui::RichText::new(DENOISE_NOTE)
                .size(12.0)
                .color(theme::T2),
        );
        let amount = env.params.map_or(50.0, |p| p.denoise.amount);
        disabled_slider(
            ui,
            "Amount",
            &format!("{amount:.0}"),
            amount / 100.0,
            DENOISE_NOTE,
        );
    } else {
        pill_button(ui, None, "Apply denoise", false).on_hover_text(DENOISE_NOTE);
    }
}

fn presence(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    develop_slider(ui, "Texture", K::Texture, env, st);
    develop_slider(ui, "Clarity", K::Clarity, env, st);
    develop_slider(ui, "Dehaze", K::Dehaze, env, st);
}

fn lens(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    for (label, key) in [
        ("Vignette", K::Vignette),
        ("Distortion", K::DistK1),
        ("Distortion · fine", K::DistK2),
        ("Defringe · red/cyan", K::CaRed),
        ("Defringe · blue/yellow", K::CaBlue),
    ] {
        develop_slider(ui, label, key, env, st);
    }
}

fn crop(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    let usable = env.enabled && env.params.is_some() && !st.show_before;
    let label = if st.crop.active {
        "Done cropping"
    } else {
        "Adjust crop"
    };
    if pill_toggle(ui, None, label, usable, st.crop.active).clicked() {
        st.crop.active = !st.crop.active;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (label, target) in crop_tool::ASPECTS {
            if pill_button(ui, None, label, usable).clicked() {
                // Legacy `setAspect`: one edit, and the tool opens even when the ratio is unknown.
                if let Some(crop) = env
                    .params
                    .and_then(|p| crop_tool::preset_crop(&p.crop, target, env.image_px))
                {
                    env.edit(|p| p.crop = crop);
                }
                st.crop.active = true;
            }
        }
    });
    ui.horizontal(|ui| {
        for (icon, label, delta) in [
            (Icon::RotateLeft, "Rotate left", 3),
            (Icon::RotateRight, "Rotate right", 1),
        ] {
            if pill_button(ui, Some(icon), label, env.enabled).clicked() {
                env.edit(|p| p.crop.rot90 = (p.crop.rot90 + delta).rem_euclid(4));
            }
        }
    });
    develop_slider(ui, "Straighten", K::CropAngle, env, st);
}
