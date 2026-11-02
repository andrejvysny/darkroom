//! The Lightroom-style develop slider and the shared edit environment. Custom painted (not
//! `egui::Slider`) with one gesture state machine for every slider in the panel:
//! drag = Begin/Update/Commit (Esc cancels and the rest of the drag is ignored), a plain click on
//! the track applies the value under the pointer, double-click resets, the value text is a
//! numeric entry, and a focused slider steps with the arrow keys.

use super::params::ControlKey;
use super::{DevelopUi, Editing};
use crate::{theme, HistogramData};
use core_app::workflow::{Command, EditIntent};
use core_model::controls::ControlSpec;
use core_model::DevelopParams;
use egui::{Align2, FontId, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, Ui};
use std::sync::Arc;

const LABEL_H: f32 = 15.0;
const TRACK_H: f32 = 14.0;
const GAP: f32 = 3.0;
const VALUE_W: f32 = 52.0;

/// What a develop widget needs to read and edit the document this frame.
pub(super) struct Env<'a> {
    pub params: Option<&'a Arc<DevelopParams>>,
    /// `ActionId::Edit` is enabled (document Ready with its image row).
    pub enabled: bool,
    pub histogram: Option<&'a HistogramData>,
    /// The selected image's catalog dims (display orientation), when known.
    pub image_px: Option<(u32, u32)>,
    pub commands: &'a mut Vec<Command>,
}

impl Env<'_> {
    pub fn intent(&mut self, intent: EditIntent) {
        self.commands.push(Command::Edit(intent));
    }

    pub fn apply(&mut self, params: DevelopParams) {
        self.intent(EditIntent::Apply(Box::new(params)));
    }

    /// One discrete `Apply` of `change(current params)`; nothing when it changes nothing.
    pub fn edit(&mut self, change: impl FnOnce(&mut DevelopParams)) {
        if !self.enabled {
            return;
        }
        let Some(base) = self.params else { return };
        let mut next = (**base).clone();
        change(&mut next);
        if next != **base {
            self.apply(next);
        }
    }
}

fn value_at(x: f32, track: Rect, spec: &ControlSpec) -> f64 {
    let t = ((x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
    spec.min + f64::from(t) * (spec.max - spec.min)
}

pub(super) fn format_value(spec: &ControlSpec, v: f64) -> String {
    let text = if spec.decimals > 0 {
        format!("{v:.*}", usize::from(spec.decimals))
    } else {
        format!("{}", v.round())
    };
    // "-0" / "-0.00" are not a value anyone means.
    let zero = text
        .trim_start_matches('-')
        .chars()
        .all(|c| c == '0' || c == '.');
    if zero {
        text.trim_start_matches('-').to_owned()
    } else if spec.bipolar && v > 0.0 {
        format!("+{text}")
    } else {
        text
    }
}

/// Paints rail, optional centre tick, accent fill (from the centre for bipolar) and the knob.
pub(super) fn paint_track(ui: &Ui, track: Rect, frac: f32, bipolar: bool, active: bool) {
    let painter = ui.painter();
    let y = track.center().y;
    let x = |t: f32| track.left() + track.width() * t;
    painter.line_segment(
        [Pos2::new(track.left(), y), Pos2::new(track.right(), y)],
        Stroke::new(2.0, theme::LINE2),
    );
    let from = if bipolar { 0.5 } else { 0.0 };
    if bipolar {
        painter.line_segment(
            [Pos2::new(x(0.5), y - 4.0), Pos2::new(x(0.5), y + 4.0)],
            Stroke::new(1.0, theme::LINE2),
        );
    }
    // Legacy always shows the accent fill; hover/drag only brightens it.
    let fill = if active {
        theme::ACCENT
    } else {
        theme::ACCENT_LINE
    };
    painter.line_segment(
        [Pos2::new(x(from), y), Pos2::new(x(frac), y)],
        Stroke::new(2.0, fill),
    );
    let knob = if active { theme::T1 } else { theme::T2 };
    painter.circle_filled(Pos2::new(x(frac), y), 6.0, theme::APP);
    painter.circle_filled(Pos2::new(x(frac), y), 4.5, knob);
}

/// Exposes a develop slider to assistive technology: a named slider with its current value,
/// bounds and keyboard step, accepting SetValue/Increment/Decrement while `enabled`.
fn declare_accessible_slider(
    ui: &Ui,
    response: &egui::Response,
    label: &str,
    spec: &ControlSpec,
    value: f64,
    text: &str,
    enabled: bool,
) {
    response.widget_info(|| egui::WidgetInfo::slider(enabled, value, label));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        use egui::accesskit::Action;
        node.set_min_numeric_value(spec.min);
        node.set_max_numeric_value(spec.max);
        node.set_numeric_value_step(keyboard_step(spec));
        // The formatted text ("+1.25") is what the panel shows; VoiceOver reads it verbatim.
        node.set_value(text);
        if !enabled {
            node.set_disabled();
            return;
        }
        node.add_action(Action::Focus);
        node.add_action(Action::SetValue);
        if value < spec.max {
            node.add_action(Action::Increment);
        }
        if value > spec.min {
            node.add_action(Action::Decrement);
        }
    });
}

/// Target value an assistive-technology request asks for this frame, if any: SetValue wins over
/// Increment/Decrement steps.
fn accessible_target(ui: &Ui, id: Id, spec: &ControlSpec, shown: f64) -> Option<f64> {
    use egui::accesskit::{Action, ActionData};
    ui.input(|input| {
        let set = input
            .accesskit_action_requests(id, Action::SetValue)
            .filter_map(|request| match request.data {
                Some(ActionData::NumericValue(v)) if v.is_finite() => Some(v),
                _ => None,
            })
            .last();
        let steps = input.num_accesskit_action_requests(id, Action::Increment) as f64
            - input.num_accesskit_action_requests(id, Action::Decrement) as f64;
        set.or((steps != 0.0).then(|| shown + steps * keyboard_step(spec)))
    })
    .map(|v| v.clamp(spec.min, spec.max))
}

/// A slider that cannot be used (placeholder): label, value text, inert track.
pub(super) fn disabled_slider(ui: &mut Ui, label: &str, value: &str, frac: f32, hover: &str) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LABEL_H + GAP + TRACK_H),
        Sense::hover(),
    );
    response.widget_info(|| egui::WidgetInfo::slider(false, f64::from(frac), label));
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_value(value);
        node.set_description(hover);
        node.set_disabled();
    });
    let top = Rect::from_min_size(rect.min, egui::vec2(rect.width(), LABEL_H));
    let painter = ui.painter();
    painter.text(
        top.left_center(),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        theme::T3,
    );
    painter.text(
        top.right_center(),
        Align2::RIGHT_CENTER,
        value,
        FontId::monospace(11.5),
        theme::T3,
    );
    let track = Rect::from_min_size(
        rect.min + egui::vec2(0.0, LABEL_H + GAP),
        egui::vec2(rect.width(), TRACK_H),
    );
    paint_track(ui, track, frac, false, false);
    response.on_hover_text(hover);
}

/// Where arrows must stay with a focused slider instead of moving focus.
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

pub(super) fn develop_slider(
    ui: &mut Ui,
    label: &str,
    key: ControlKey,
    env: &mut Env<'_>,
    st: &mut DevelopUi,
) {
    let spec = key.spec();
    let shown = env.params.map_or(key.reset_ui(), |p| key.display(p));
    let id = Id::unique(("develop_slider", key));
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LABEL_H + GAP + TRACK_H),
        Sense::hover(),
    );
    let label_rect = Rect::from_min_size(rect.min, egui::vec2(rect.width() - VALUE_W, LABEL_H));
    let value_rect = Rect::from_min_size(
        Pos2::new(rect.right() - VALUE_W, rect.top()),
        egui::vec2(VALUE_W, LABEL_H),
    );
    let track = Rect::from_min_size(
        rect.min + egui::vec2(0.0, LABEL_H + GAP),
        egui::vec2(rect.width(), TRACK_H),
    );
    st.slider_tracks.insert(key.name(), track);
    st.slider_values.insert(key.name(), value_rect);
    if key == ControlKey::Exposure {
        st.exposure_rect = Some(track);
    }

    let frac = ((shown - spec.min) / (spec.max - spec.min)).clamp(0.0, 1.0) as f32;
    let live = env.enabled && env.params.is_some();
    let label_color = if live { theme::T2 } else { theme::T3 };
    ui.painter().text(
        label_rect.left_center(),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.0),
        label_color,
    );

    if !live {
        if st.gesture.is_some() || st.cancelled {
            st.gesture = None;
            st.cancelled = false;
        }
        if st.editing.as_ref().is_some_and(|e| e.id == id) {
            st.editing = None;
        }
        ui.painter().text(
            value_rect.right_center(),
            Align2::RIGHT_CENTER,
            format_value(spec, shown),
            FontId::monospace(11.5),
            theme::T3,
        );
        paint_track(ui, track, frac, spec.bipolar, false);
        let inert = ui.interact(track, id, Sense::hover());
        declare_accessible_slider(
            ui,
            &inert,
            label,
            spec,
            shown,
            &format_value(spec, shown),
            false,
        );
        return;
    }

    let response = ui.interact(track, id, Sense::click_and_drag());
    declare_accessible_slider(
        ui,
        &response,
        label,
        spec,
        shown,
        &format_value(spec, shown),
        true,
    );
    let label_response = ui.interact(label_rect, id.with("label"), Sense::click());
    label_response
        .widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Label, true, label));
    let editing_here = st.editing.as_ref().is_some_and(|e| e.id == id);
    paint_track(
        ui,
        track,
        frac,
        spec.bipolar,
        response.hovered() || response.dragged() || response.has_focus(),
    );
    if editing_here {
        numeric_entry(ui, id, value_rect, key, env, st);
    } else {
        let value_response = ui.interact(value_rect, id.with("value"), Sense::click());
        value_response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::accesskit::Role::Button,
                true,
                format!("Edit {label} value"),
            )
        });
        let color = if value_response.hovered() {
            theme::ACCENT
        } else {
            theme::T1
        };
        ui.painter().text(
            value_rect.right_center(),
            Align2::RIGHT_CENTER,
            format_value(spec, shown),
            FontId::monospace(11.5),
            color,
        );
        if value_response.clicked() {
            let mut text = format_value(spec, shown);
            text.retain(|c| c != '+');
            st.editing = Some(Editing {
                id,
                text,
                fresh: true,
            });
        }
    }
    if response.has_focus() {
        keep_arrows(ui, response.id);
    }
    track_interaction(ui, &response, key, env, st, shown);
    if label_response.double_clicked() || response.double_clicked() {
        env.edit(|p| key.reset(p));
    }
}

/// Drag gesture, track click, and keyboard stepping for one slider.
fn track_interaction(
    ui: &mut Ui,
    response: &egui::Response,
    key: ControlKey,
    env: &mut Env<'_>,
    st: &mut DevelopUi,
    shown: f64,
) {
    let spec = key.spec();
    let id = response.id;
    let pointer_x = response.interact_pointer_pos().map(|p| p.x);
    if response.drag_started() && st.gesture.is_none() {
        st.cancelled = false;
        st.gesture = Some(id);
        st.last_sent = None;
        env.intent(EditIntent::BeginGesture);
    }
    if st.gesture == Some(id) && ui.input(|i| i.key_pressed(Key::Escape)) {
        st.gesture = None;
        st.cancelled = true;
        env.intent(EditIntent::CancelGesture);
    }
    if response.dragged() && st.gesture == Some(id) {
        if let (Some(x), Some(base)) = (pointer_x, env.params) {
            let v = value_at(x, response.rect, spec);
            if st.last_sent != Some(v) {
                if let Some(next) = key.with_ui(base, v) {
                    st.last_sent = Some(v);
                    env.intent(EditIntent::UpdateGesture(Box::new(next)));
                }
            }
        }
    }
    if response.drag_stopped() {
        if st.gesture == Some(id) {
            st.gesture = None;
            env.intent(EditIntent::CommitGesture);
        }
        st.cancelled = false;
    }
    // A pointer click does not take keyboard focus: arrows must keep stepping through photos.
    // Keyboard nudging is for sliders reached with Tab.
    if response.clicked() && !response.double_clicked() {
        if let (Some(x), Some(base)) = (pointer_x, env.params) {
            if let Some(next) = key.with_ui(base, value_at(x, response.rect, spec)) {
                env.apply(next);
            }
        }
    }
    if response.has_focus() && st.editing.is_none() {
        arrow_step(ui, key, env, shown);
    }
    // Assistive-technology adjustments are discrete edits like an arrow step.
    if st.gesture != Some(id) {
        if let (Some(target), Some(base)) = (accessible_target(ui, id, spec, shown), env.params) {
            if let Some(next) = key.with_ui(base, target) {
                env.apply(next);
            }
        }
    }
}

/// One keyboard (and assistive Increment/Decrement) step: 1/200 of the range.
fn keyboard_step(spec: &ControlSpec) -> f64 {
    (spec.max - spec.min) / 200.0
}

/// Left/Right = (max-min)/200, Shift = x10, as one discrete `Apply`.
fn arrow_step(ui: &mut Ui, key: ControlKey, env: &mut Env<'_>, shown: f64) {
    let spec = key.spec();
    let step = keyboard_step(spec);
    for (arrow, direction) in [(Key::ArrowLeft, -1.0), (Key::ArrowRight, 1.0)] {
        let factor = ui.input_mut(|i| {
            if i.consume_key(Modifiers::SHIFT, arrow) {
                10.0
            } else if i.consume_key(Modifiers::NONE, arrow) {
                1.0
            } else {
                0.0
            }
        });
        if factor > 0.0 {
            let target = (shown + direction * step * factor).clamp(spec.min, spec.max);
            if let Some(base) = env.params {
                if let Some(next) = key.with_ui(base, target) {
                    env.apply(next);
                }
            }
            return;
        }
    }
}

/// Inline numeric entry over the value text: Enter applies (clamped to the range), Esc cancels.
fn numeric_entry(
    ui: &mut Ui,
    id: Id,
    rect: Rect,
    key: ControlKey,
    env: &mut Env<'_>,
    st: &mut DevelopUi,
) {
    let Some(editing) = st.editing.as_mut() else {
        return;
    };
    let edit_id = id.with("entry");
    // A child at a fixed rect: `Ui::put` would advance the parent's cursor and shift the rows below.
    let mut child =
        ui.new_child(egui::UiBuilder::new().max_rect(rect.expand2(egui::vec2(8.0, 1.0))));
    let output = child.add(
        egui::TextEdit::singleline(&mut editing.text)
            .id(edit_id)
            .font(FontId::monospace(11.5))
            .align(egui::Align2::RIGHT_CENTER)
            .margin(egui::Margin::symmetric(3, 0)),
    );
    if editing.fresh {
        // Focus lands a frame after the request; select the text once it has.
        if !output.has_focus() {
            output.request_focus();
            return;
        }
        editing.fresh = false;
        if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), edit_id) {
            let end = egui::text::CCursor::new(editing.text.chars().count());
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    end,
                )));
            state.store(ui.ctx(), edit_id);
        }
        return;
    }
    if !output.lost_focus() {
        return;
    }
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    let enter = ui.input(|i| i.key_pressed(Key::Enter));
    let text = editing.text.clone();
    st.editing = None;
    if escape || !enter {
        return;
    }
    let spec = key.spec();
    if let Ok(typed) = text.trim().replace(',', ".").parse::<f64>() {
        if typed.is_finite() {
            if let Some(base) = env.params {
                if let Some(next) = key.with_ui(base, typed.clamp(spec.min, spec.max)) {
                    env.apply(next);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_value;
    use crate::develop::params::ControlKey;

    #[test]
    fn values_format_like_the_legacy_slider() {
        let exposure = ControlKey::Exposure.spec();
        assert_eq!(format_value(exposure, 0.0), "0.00");
        assert_eq!(format_value(exposure, 1.234), "+1.23");
        assert_eq!(format_value(exposure, -0.5), "-0.50");
        let contrast = ControlKey::Contrast.spec();
        assert_eq!(format_value(contrast, 12.4), "+12");
        assert_eq!(format_value(contrast, -0.2), "0");
        let sharpen = ControlKey::Sharpen.spec();
        assert_eq!(format_value(sharpen, 40.0), "40");
    }
}
