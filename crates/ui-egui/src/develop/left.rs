//! Left rail: Presets | History tabs (collapsible). Both are honest placeholders — the native app
//! has neither presets nor an edit-history list yet — so every control is drawn disabled and no
//! preset is ever listed.

use super::slider::disabled_slider;
use super::DevelopUi;
use crate::icons::{icon_button, icon_button_enabled, Icon};
use crate::widgets::pill_button;
use crate::{theme, NOT_YET};
use egui::{Margin, RichText, Sense, Stroke, Ui};

pub const LEFT_PANEL_ID: &str = "develop_left";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Presets,
    History,
}

pub(super) fn show(root: &mut Ui, st: &mut DevelopUi) {
    let frame = egui::Frame::new().fill(theme::APP);
    if st.left_collapsed {
        egui::Panel::left("develop_left_collapsed")
            .exact_size(28.0)
            .resizable(false)
            .frame(frame)
            .show(root, |ui| {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    if icon_button(ui, Icon::ChevronRight, "Show presets & history").clicked() {
                        st.left_collapsed = false;
                    }
                });
            });
        return;
    }
    egui::Panel::left(LEFT_PANEL_ID)
        .resizable(true)
        .default_size(190.0)
        .size_range(170.0..=300.0)
        .frame(frame)
        .show(root, |ui| {
            tab_bar(ui, st);
            egui::Frame::new()
                .inner_margin(Margin::symmetric(12, 4))
                .show(ui, |ui| match st.left_tab {
                    Tab::Presets => presets(ui),
                    Tab::History => note(ui, "History isn't in the native app yet."),
                });
        });
}

fn tab_bar(ui: &mut Ui, st: &mut DevelopUi) {
    let height = 34.0;
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
    let collapse_w = 28.0;
    let tab_w = (rect.width() - collapse_w) / 2.0;
    for (i, (tab, label)) in [(Tab::Presets, "Presets"), (Tab::History, "History")]
        .into_iter()
        .enumerate()
    {
        let r = egui::Rect::from_min_size(
            rect.min + egui::vec2(tab_w * i as f32, 0.0),
            egui::vec2(tab_w, height),
        );
        let response = ui.interact(
            r,
            ui.scope_id().with(("develop_left_tab", i)),
            Sense::click(),
        );
        let on = st.left_tab == tab;
        ui.painter().text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            if on { theme::T1 } else { theme::T3 },
        );
        if on {
            ui.painter().hline(
                r.x_range(),
                r.bottom() - 1.0,
                Stroke::new(2.0, theme::ACCENT),
            );
        }
        if response.clicked() {
            st.left_tab = tab;
        }
    }
    let collapse = egui::Rect::from_min_size(
        egui::pos2(rect.right() - collapse_w, rect.top() + 4.0),
        egui::vec2(collapse_w, 26.0),
    );
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(collapse));
    if icon_button(&mut child, Icon::ChevronLeft, "Hide panel").clicked() {
        st.left_collapsed = true;
    }
    ui.painter()
        .hline(rect.x_range(), rect.bottom(), Stroke::new(1.0, theme::LINE));
}

fn note(ui: &mut Ui, text: &str) {
    ui.add_space(8.0);
    ui.add(egui::Label::new(RichText::new(text).italics().size(12.0).color(theme::T3)).wrap());
}

fn presets(ui: &mut Ui) {
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
    disabled_slider(ui, "Amount", "100", 1.0, NOT_YET);
    ui.horizontal_wrapped(|ui| {
        pill_button(ui, None, "Copy settings", false).on_hover_text(NOT_YET);
        pill_button(ui, None, "Paste", false).on_hover_text(NOT_YET);
    });
    theme::section_heading(ui, "Built-in");
    note(ui, "Presets aren't in the native app yet.");
}
