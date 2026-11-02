//! Bottom toolbar: count, filters, sort, zoom, view toggle, pagination.

use super::filters::{any_filter, clear_filters};
use super::reject_dialog::RejectDialog;
use super::{requery, LibraryUi, TOOLBAR_ID};
use crate::icons::{self, icon_button, Icon};
use crate::theme;
use crate::widgets::{chip, color_dots, fmt_count, pill_button, star_rating};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{ColorLabel, Command};
use egui::{Align, Layout, Rect, RichText, Sense, Stroke, StrokeKind};

const SORTS: [(&str, &str); 8] = [
    ("capture_asc", "Capture date ↑"),
    ("capture_desc", "Capture date ↓"),
    ("filename", "Filename A→Z"),
    ("filename_desc", "Filename Z→A"),
    ("rating_desc", "Rating ↓"),
    ("rating_asc", "Rating ↑"),
    ("imported_desc", "Imported (newest)"),
    ("imported_asc", "Imported (oldest)"),
];

pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    egui::Panel::bottom(TOOLBAR_ID)
        .exact_size(40.0)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::APP)
                .inner_margin(egui::Margin::symmetric(12, 0)),
        )
        .show(root, |ui| {
            // Below this width the full right group would overlap the filters (minimum window).
            let compact = ui.available_width() < COMPACT_BELOW;
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                filters(ui, snapshot, lib, commands);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    view_toggle(ui, lib);
                    if !compact {
                        zoom(ui, lib);
                    }
                    sort(ui, snapshot, commands, compact);
                    pagination(ui, snapshot, commands);
                });
            });
        });
}

/// Toolbar width (points) under which the zoom slider and the "Sort" label are dropped.
const COMPACT_BELOW: f32 = 820.0;

fn filters(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    let query = &snapshot.library.query;
    ui.label(
        RichText::new(format!(
            "{} {}",
            fmt_count(snapshot.library.count),
            if snapshot.library.count == 1 {
                "photo"
            } else {
                "photos"
            }
        ))
        .monospace()
        .color(theme::T2),
    );
    ui.add_space(8.0);
    let min = u8::try_from(query.min_stars.unwrap_or(0).clamp(0, 5)).unwrap_or(0);
    if let Some(n) = star_rating(ui, min, 12.0, true) {
        commands.push(requery(snapshot, |q| {
            q.min_stars = (n > 0).then_some(i64::from(n));
        }));
    }
    for (label, value) in [("Picks", "pick"), ("Rejects", "reject")] {
        let active = query.flag.as_deref() == Some(value);
        if chip(ui, label, active).clicked() {
            commands.push(requery(snapshot, |q| {
                q.flag = if active { None } else { Some(value.to_owned()) };
            }));
        }
    }
    let current = query.color_label.as_deref().and_then(ColorLabel::from_row);
    if let Some(next) = color_dots(ui, current, 9.0) {
        commands.push(requery(snapshot, |q| {
            q.color_label = next.map(|l| l.as_str().to_owned());
        }));
    }
    let unlabeled = query.color_label.as_deref() == Some(NO_LABEL);
    if chip(ui, "No label", unlabeled).clicked() {
        commands.push(requery(snapshot, |q| {
            q.color_label = (!unlabeled).then(|| NO_LABEL.to_owned());
        }));
    }
    if any_filter(query)
        && chip(ui, "Clear", false)
            .on_hover_text("Clear all filters")
            .clicked()
    {
        commands.push(requery(snapshot, clear_filters));
    }
    if query.flag.as_deref() == Some("reject") && snapshot.library.count > 0 {
        let ready = snapshot.catalog_ready && super::mutate_enabled(snapshot);
        let response = pill_button(ui, Some(Icon::Trash), "Delete rejected…", ready);
        lib.button_rects.push(("delete_rejected", response.rect));
        if response.clicked() {
            lib.reject_dialog = Some(RejectDialog::new(query));
        }
    }
}

/// `QueryParams::color_label` sentinel for "no colour label".
const NO_LABEL: &str = "__none__";

fn sort(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    commands: &mut Vec<Command>,
    compact: bool,
) {
    let current = snapshot
        .library
        .query
        .sort
        .as_deref()
        .unwrap_or("capture_desc");
    let shown = SORTS
        .iter()
        .find(|(key, _)| *key == current)
        .map_or(current, |(_, l)| *l);
    // Right-to-left layout: the combo is added first so the "Sort" label ends up on its left.
    egui::ComboBox::from_id_salt("library_sort")
        .selected_text(shown)
        .width(if compact { 110.0 } else { 140.0 })
        .show_ui(ui, |ui| {
            for (key, label) in SORTS {
                if ui.selectable_label(current == key, label).clicked() {
                    commands.push(requery(snapshot, |q| q.sort = Some(key.to_owned())));
                }
            }
        });
    if !compact {
        ui.label(RichText::new("Sort").color(theme::T3));
    }
}

fn zoom(ui: &mut egui::Ui, lib: &mut LibraryUi) {
    ui.add_space(8.0);
    // Right-to-left: large icon, slider, small icon.
    tiny_icon(ui, Icon::Square);
    ui.push_id("library_zoom", |ui| {
        ui.spacing_mut().slider_width = 110.0;
        ui.add(egui::Slider::new(&mut lib.cell_size, 100.0..=320.0).show_value(false));
    });
    tiny_icon(ui, Icon::Grid);
}

fn tiny_icon(ui: &mut egui::Ui, icon: Icon) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), Sense::hover());
    icons::paint(ui.painter(), rect, icon, theme::T3);
}

fn view_toggle(ui: &mut egui::Ui, lib: &mut LibraryUi) {
    // Right-to-left: loupe then grid.
    for (loupe, icon, tip) in [(true, Icon::Square, "Loupe"), (false, Icon::Grid, "Grid")] {
        let active = lib.loupe == loupe;
        let (rect, response) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), Sense::click());
        crate::widgets::describe_selected(
            &response,
            egui::accesskit::Role::Button,
            true,
            active,
            tip,
        );
        crate::widgets::focus_ring(ui, &response, rect, theme::RADIUS_SM);
        if active {
            ui.painter()
                .rect_filled(rect, theme::RADIUS_SM, theme::ELEV);
            ui.painter().rect_stroke(
                rect,
                theme::RADIUS_SM,
                Stroke::new(1.0, theme::LINE2),
                StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter()
                .rect_filled(rect, theme::RADIUS_SM, theme::HOVER.gamma_multiply(0.6));
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(rect.center(), egui::vec2(15.0, 15.0)),
            icon,
            if active { theme::T1 } else { theme::T3 },
        );
        if response.on_hover_text(tip).clicked() {
            lib.loupe = loupe;
        }
    }
}

/// `‹ 1–5,000 of 12,345 ›` when the count exceeds the loaded page.
fn pagination(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot, commands: &mut Vec<Command>) {
    let query = &snapshot.library.query;
    let offset = query.offset.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(5000).max(1);
    let loaded = i64::try_from(snapshot.library.rows.len()).unwrap_or(i64::MAX);
    if snapshot.library.count <= loaded && offset == 0 {
        return;
    }
    let end = offset.saturating_add(loaded);
    let first = if loaded == 0 { 0 } else { offset + 1 };
    let ready = snapshot.catalog_ready;
    // Right-to-left: next, range text, previous.
    let next = (end < snapshot.library.count).then(|| offset.saturating_add(limit));
    let prev = (offset > 0).then(|| offset.saturating_sub(limit).max(0));
    let page = |ui: &mut egui::Ui, icon, tip, target: Option<i64>| -> Option<i64> {
        let clicked = ui
            .add_enabled_ui(ready && target.is_some(), |ui| {
                icon_button(ui, icon, tip).clicked()
            })
            .inner;
        target.filter(|_| clicked)
    };
    let mut picked = page(ui, Icon::ChevronRight, "Next page", next);
    ui.label(
        RichText::new(format!(
            "{}–{} of {}",
            fmt_count(first),
            fmt_count(end),
            fmt_count(snapshot.library.count)
        ))
        .monospace()
        .color(theme::T2),
    );
    picked = picked.or(page(ui, Icon::ChevronLeft, "Previous page", prev));
    if let Some(target) = picked {
        let mut query = query.clone();
        query.offset = Some(target);
        query.limit = Some(limit);
        query.cursor_value = None;
        query.cursor_id = None;
        commands.push(Command::QueryLibrary(Box::new(query)));
    }
}
