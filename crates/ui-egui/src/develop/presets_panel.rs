//! Presets tab: the shared Amount, Copy/Paste settings and the preset list. Applying never edits
//! here: it emits `ApplySettings`, the application resolves it against the open document. Creating,
//! renaming, deleting and importing presets are not part of this panel yet.

use super::{enabled_context, DevelopUi};
use crate::develop::slider::paint_track;
use crate::icons::{icon_button_enabled, Icon};
use crate::widgets::{focus_ring, pill_button};
use crate::{theme, NOT_YET};
use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, SettingsSource};
use core_model::DevelopParams;
use egui::accesskit::{Action, ActionData};
use egui::{Align2, FontId, Id, Pos2, Rect, RichText, Sense, Ui};
use std::sync::Arc;

const DEFAULT_AMOUNT: f32 = 100.0;
const TOAST_SECS: f64 = 2.0;
const LABEL_H: f32 = 15.0;
const TRACK_H: f32 = 14.0;
const GAP: f32 = 3.0;
const ROW_H: f32 = 24.0;

/// Transient state of the Presets tab; it outlives mode and photo switches (a copy made on one
/// photo is pasted on another).
#[derive(Default)]
pub(super) struct PresetsUi {
    /// Settings captured by Copy settings.
    copied: Option<Arc<DevelopParams>>,
    /// Amount in percent; `None` is the default 100.
    amount: Option<f32>,
    /// Message and the egui time it was raised.
    toast: Option<(&'static str, f64)>,
}

impl PresetsUi {
    /// Amount as the 0..=1 fraction `ApplySettings` takes.
    pub(super) fn fraction(&self) -> f32 {
        self.amount.unwrap_or(DEFAULT_AMOUNT) / 100.0
    }
}

pub(super) fn show(
    ui: &mut Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    commands: &mut Vec<Command>,
) {
    let can_edit = action(ActionId::ApplySettings)
        .enabled
        .is_enabled(enabled_context(snapshot));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("PRESETS")
                .size(10.5)
                .color(theme::T3)
                .extra_letter_spacing(1.0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            icon_button_enabled(ui, Icon::Plus, NOT_YET, false);
            icon_button_enabled(ui, Icon::Download, NOT_YET, false);
        });
    });
    amount_slider(ui, &mut st.presets.amount);
    ui.add_space(4.0);
    copy_paste(ui, snapshot, st, can_edit, commands);
    let presets = &snapshot.library.presets;
    if presets.is_empty() {
        ui.add_space(8.0);
        ui.add(
            egui::Label::new(
                RichText::new("No presets.")
                    .italics()
                    .size(12.0)
                    .color(theme::T3),
            )
            .wrap(),
        );
    }
    let mut group: Option<&str> = None;
    for preset in presets.iter() {
        if group != Some(preset.group_name.as_str()) {
            theme::section_heading(ui, &preset.group_name);
            group = Some(preset.group_name.as_str());
        }
        if list_row(ui, &preset.name, can_edit, ui.available_width()).clicked() {
            commands.push(Command::ApplySettings {
                source: SettingsSource::Preset(preset.id),
                amount: st.presets.fraction(),
                replace_all: false,
            });
        }
    }
}

fn copy_paste(
    ui: &mut Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    can_edit: bool,
    commands: &mut Vec<Command>,
) {
    let now = ui.input(|i| i.time);
    ui.horizontal_wrapped(|ui| {
        if pill_button(ui, None, "Copy settings", can_edit).clicked() {
            if let Some(params) = snapshot.editor.document.params.clone() {
                st.presets.copied = Some(params);
                st.presets.toast = Some(("Settings copied", now));
            }
        }
        let paste = pill_button(ui, None, "Paste", can_edit && st.presets.copied.is_some());
        if paste.clicked() {
            if let Some(params) = st.presets.copied.clone() {
                commands.push(Command::ApplySettings {
                    source: SettingsSource::Copied {
                        params,
                        field_keys: None,
                    },
                    amount: st.presets.fraction(),
                    replace_all: false,
                });
            }
        }
        if st.presets.copied.is_none() {
            paste.on_hover_text("Copy settings first");
        }
    });
}

/// Brief confirmation near the bottom-left corner; clears itself after [`TOAST_SECS`].
pub(super) fn toast(ctx: &egui::Context, st: &mut DevelopUi) {
    let Some((message, since)) = st.presets.toast else {
        return;
    };
    let now = ctx.input(|i| i.time);
    if now - since > TOAST_SECS {
        st.presets.toast = None;
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
    egui::Area::new(Id::unique("settings_toast"))
        .order(egui::Order::Tooltip)
        .anchor(Align2::LEFT_BOTTOM, [14.0, -14.0])
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(theme::ELEV)
                .stroke(egui::Stroke::new(1.0, theme::LINE2))
                .corner_radius(theme::RADIUS_MD)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.label(RichText::new(message).color(theme::T1));
                });
        });
}

/// A full-width text row that behaves as a button; inert (and announced disabled) unless `enabled`.
pub(super) fn list_row(ui: &mut Ui, label: &str, enabled: bool, width: f32) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, ROW_H), sense);
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Button, enabled, label));
    focus_ring(ui, &response, rect, theme::RADIUS_SM);
    if enabled && response.hovered() {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_SM, theme::HOVER.gamma_multiply(0.6));
    }
    let color = match (enabled, response.hovered()) {
        (false, _) => theme::T3,
        (true, true) => theme::T1,
        (true, false) => theme::T2,
    };
    ui.painter().with_clip_rect(rect).text(
        Pos2::new(rect.left() + 10.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        color,
    );
    response
}

/// 0..=100 slider over the track painter the develop sliders use. Click/drag sets the value,
/// arrows nudge it, assistive technology can set or step it.
fn amount_slider(ui: &mut Ui, amount: &mut Option<f32>) {
    let shown = amount.unwrap_or(DEFAULT_AMOUNT);
    let id = Id::unique("develop_preset_amount");
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LABEL_H + GAP + TRACK_H),
        Sense::hover(),
    );
    let top = Rect::from_min_size(rect.min, egui::vec2(rect.width(), LABEL_H));
    let track = Rect::from_min_size(
        rect.min + egui::vec2(0.0, LABEL_H + GAP),
        egui::vec2(rect.width(), TRACK_H),
    );
    let response = ui.interact(track, id, Sense::click_and_drag());
    let mut next = shown;
    if let (true, Some(pos)) = (
        response.dragged() || response.clicked(),
        response.interact_pointer_pos(),
    ) {
        let t = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
        next = (t * 100.0).round();
    }
    if response.has_focus() {
        keep_arrows(ui, id);
        next = arrow_target(ui, next);
    }
    next = accessible_target(ui, id, next).unwrap_or(next);
    if next != shown {
        *amount = Some(next);
    }
    let value = format!("{}", next.round());
    declare_accessible(ui, &response, next, &value);
    let painter = ui.painter();
    painter.text(
        top.left_center(),
        Align2::LEFT_CENTER,
        "Amount",
        FontId::proportional(12.0),
        theme::T2,
    );
    painter.text(
        top.right_center(),
        Align2::RIGHT_CENTER,
        &value,
        FontId::monospace(11.5),
        theme::T1,
    );
    paint_track(
        ui,
        track,
        next / 100.0,
        false,
        response.hovered() || response.dragged() || response.has_focus(),
    );
}

/// Arrows must stay with a focused slider instead of moving focus.
fn keep_arrows(ui: &Ui, id: Id) {
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(
            id,
            egui::EventFilter {
                horizontal_arrows: true,
                ..Default::default()
            },
        );
    });
}

/// Left/Right = 1, Shift = 10.
fn arrow_target(ui: &mut Ui, value: f32) -> f32 {
    for (arrow, direction) in [(egui::Key::ArrowLeft, -1.0), (egui::Key::ArrowRight, 1.0)] {
        let factor = ui.input_mut(|i| {
            if i.consume_key(egui::Modifiers::SHIFT, arrow) {
                10.0
            } else if i.consume_key(egui::Modifiers::NONE, arrow) {
                1.0
            } else {
                0.0
            }
        });
        if factor > 0.0 {
            return (value + direction * factor).clamp(0.0, 100.0);
        }
    }
    value
}

fn declare_accessible(ui: &Ui, response: &egui::Response, value: f32, text: &str) {
    response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(value), "Amount"));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(0.0);
        node.set_max_numeric_value(100.0);
        node.set_numeric_value_step(1.0);
        node.set_value(text);
        node.add_action(Action::Focus);
        node.add_action(Action::SetValue);
        if value < 100.0 {
            node.add_action(Action::Increment);
        }
        if value > 0.0 {
            node.add_action(Action::Decrement);
        }
    });
}

/// SetValue wins over Increment/Decrement steps, as for the develop sliders.
fn accessible_target(ui: &Ui, id: Id, shown: f32) -> Option<f32> {
    ui.input(|input| {
        let set = input
            .accesskit_action_requests(id, Action::SetValue)
            .filter_map(|request| match request.data {
                Some(ActionData::NumericValue(v)) if v.is_finite() => Some(v as f32),
                _ => None,
            })
            .last();
        let steps = input.num_accesskit_action_requests(id, Action::Increment) as f32
            - input.num_accesskit_action_requests(id, Action::Decrement) as f32;
        set.or((steps != 0.0).then_some(shown + steps))
    })
    .map(|v| v.clamp(0.0, 100.0).round())
}
