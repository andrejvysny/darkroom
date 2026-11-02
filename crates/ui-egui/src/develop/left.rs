//! Left rail: Presets | History tabs (collapsible). This file only owns the tab strip and the
//! collapse state; the tab bodies are `presets_panel` and `history_panel`.

use super::{history_panel, presets_panel, DevelopUi};
use crate::icons::{icon_button, Icon};
use crate::theme;
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use egui::{Margin, Sense, Stroke, Ui};

pub const LEFT_PANEL_ID: &str = "develop_left";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Presets,
    History,
}

pub(super) fn show(
    root: &mut Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    commands: &mut Vec<Command>,
) {
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
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("develop_left_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| match st.left_tab {
                            Tab::Presets => presets_panel::show(ui, snapshot, st, commands),
                            Tab::History => history_panel::show(ui, snapshot, st, commands),
                        });
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
        crate::widgets::describe_selected(&response, egui::accesskit::Role::Tab, true, on, label);
        crate::widgets::focus_ring(ui, &response, r, 0);
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
