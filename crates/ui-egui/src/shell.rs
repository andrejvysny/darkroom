//! App shell: top bar (mode tabs, search, catalog menu) and the command-error strip.

use crate::icons::{icon_button, Icon};
use crate::library::{requery, selected_row, NOT_YET};
use crate::widgets::{pill_button, pill_toggle, segmented};
use crate::{theme, Mode, UiState};
use core_app::export::ExportRequest;
use core_app::import::ImportRequest;
use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use egui::{Align, Align2, FontId, Layout, Margin, Pos2, Rect, RichText, Stroke, StrokeKind};
use std::time::{Duration, Instant};

pub const TOP_BAR_ID: &str = "shell_top_bar";
pub const COMMAND_ERROR_ID: &str = "shell_command_error";
/// Id salt of the Library search box.
pub const SEARCH_ID: &str = "library_search";
const SEARCH_DEBOUNCE: f64 = 0.3;
const MODES: [(&str, Mode); 3] = [
    ("Library", Mode::Library),
    ("Develop", Mode::Develop),
    ("Duplicates", Mode::Duplicates),
];

pub(crate) fn top_bar(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    reopen: &Command,
    state: &mut UiState,
    commands: &mut Vec<Command>,
) {
    let left = state.chrome_inset_left.clamp(12.0, 120.0) as i8;
    let frame = egui::Frame::new().fill(theme::APP).inner_margin(Margin {
        left,
        right: 14,
        top: 0,
        bottom: 0,
    });
    egui::Panel::top(TOP_BAR_ID)
        .exact_size(46.0)
        .resizable(false)
        .frame(frame)
        .show(root, |ui| {
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                let selected = MODES
                    .iter()
                    .position(|(_, m)| *m == state.mode)
                    .unwrap_or(0);
                let labels = MODES.map(|(label, _)| label);
                if let Some(i) = segmented(ui, &labels, selected) {
                    state.mode = MODES[i].1;
                }
                if state.mode == Mode::Library {
                    search(ui, snapshot, state, commands);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    right_side(ui, snapshot, reopen, state, commands);
                });
            });
            if state.mode == Mode::Develop {
                develop_title(ui, snapshot);
            }
        });
}

fn right_side(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    reopen: &Command,
    state: &mut UiState,
    commands: &mut Vec<Command>,
) {
    let mode = state.mode;
    // Right-to-left: the ⌘K chip first, so it ends up rightmost.
    let chip = pill_button(ui, None, "⌘K", false);
    chip.on_hover_text(NOT_YET);
    let gear = icon_button(ui, Icon::Gear, "Catalog");
    if let Some(menu) = egui::Popup::menu(&gear).show(|ui| {
        let context = crate::develop::enabled_context(snapshot);
        let mut picked = None;
        for (id, command) in [
            (ActionId::OpenCatalog, reopen.clone()),
            (ActionId::RefreshLibrary, Command::RefreshLibrary),
            (ActionId::CloseCatalog, Command::CloseCatalog),
        ] {
            let descriptor = action(id);
            let enabled = descriptor.enabled.is_enabled(context);
            if ui
                .add_enabled(enabled, egui::Button::new(descriptor.label))
                .clicked()
            {
                picked = Some(command);
            }
        }
        picked
    }) {
        commands.extend(menu.inner);
    }
    if mode != Mode::Develop {
        let enabled = snapshot.catalog_ready;
        if pill_button(ui, Some(Icon::Download), "Import", enabled)
            .on_hover_text("Import photos from a folder or card")
            .clicked()
            && enabled
        {
            state.import_requests.push(ImportRequest::Open);
        }
    }
    let ids = crate::export_dialog::export_ids(snapshot, mode, &state.library);
    let can_export = snapshot.catalog_ready && !ids.is_empty();
    if pill_button(ui, Some(Icon::Upload), "Export", can_export)
        .on_hover_text("Export the selected photos (⇧⌘E)")
        .clicked()
        && can_export
    {
        state.export_requests.push(ExportRequest::Open(ids));
    }
    if mode == Mode::Develop {
        let reset = crate::develop::reset_all_command(snapshot);
        let hover = if reset.is_some() {
            "Reset every adjustment to its default"
        } else {
            "Nothing to reset"
        };
        let clicked = pill_button(ui, Some(Icon::Reset), "Reset", reset.is_some())
            .on_hover_text(hover)
            .clicked();
        if let (true, Some(command)) = (clicked, reset) {
            commands.push(command);
        }
        let compare = pill_toggle(
            ui,
            Some(Icon::Compare),
            "Before / After",
            snapshot.editor.selected.is_some(),
            state.develop.before_shown(),
        )
        .on_hover_text("Show the unedited photo (\\)");
        if compare.clicked() {
            state.develop.toggle_before(commands);
        }
    }
    if snapshot.jobs.metrics.queued + snapshot.jobs.metrics.running > 0 {
        ui.spinner();
    }
}

/// Search box: Enter or 300 ms after the last keystroke dispatches the query.
fn search(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    state: &mut UiState,
    commands: &mut Vec<Command>,
) {
    let lib = &mut state.library;
    ui.spacing_mut().item_spacing.x = 6.0;
    let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    crate::icons::paint(ui.painter(), icon_rect, Icon::Search, theme::T3);
    let edit = egui::TextEdit::singleline(&mut lib.search)
        .id(egui::Id::unique(SEARCH_ID))
        .hint_text("Search filename, camera, keyword…")
        .desired_width(300.0)
        .margin(Margin::symmetric(8, 4));
    let response = ui.add(edit);
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label("Search library");
    });
    let now = ui.input(|i| i.time);
    if response.changed() {
        lib.search_edited_at = Some(now);
    }
    let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let due = lib
        .search_edited_at
        .is_some_and(|t| now - t >= SEARCH_DEBOUNCE);
    if enter || due {
        lib.search_edited_at = None;
        let wanted = Some(lib.search.trim().to_owned()).filter(|s| !s.is_empty());
        if wanted != snapshot.library.query.search {
            commands.push(requery(snapshot, |q| q.search = wanted));
        }
    } else if lib.search_edited_at.is_some() {
        ui.ctx().request_repaint_after(Duration::from_millis(100));
    }
}

/// Develop mode: centred filename and the pipeline badge.
fn develop_title(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot) {
    let row = selected_row(snapshot);
    let name = row.map_or("—", |r| r.filename.as_str());
    let badge = format_badge(row.and_then(|r| r.format.as_deref()).unwrap_or("raw"));
    let painter = ui.painter();
    let name_galley = painter.layout_no_wrap(name.to_owned(), FontId::monospace(12.5), theme::T1);
    let badge_galley = painter.layout_no_wrap(badge, FontId::proportional(10.0), theme::ACCENT);
    let (name_w, badge_w) = (name_galley.size().x, badge_galley.size().x + 12.0);
    let center = ui.max_rect().center();
    let x0 = center.x - (name_w + 9.0 + badge_w) * 0.5;
    painter.galley(
        Pos2::new(x0, center.y - name_galley.size().y * 0.5),
        name_galley,
        theme::T1,
    );
    let badge_rect = Rect::from_min_size(
        Pos2::new(x0 + name_w + 9.0, center.y - 9.0),
        egui::vec2(badge_w, 18.0),
    );
    painter.rect_stroke(
        badge_rect,
        4,
        Stroke::new(1.0, theme::ACCENT_LINE),
        StrokeKind::Inside,
    );
    painter.galley(
        Pos2::new(
            badge_rect.left() + 6.0,
            badge_rect.center().y - badge_galley.size().y * 0.5,
        ),
        badge_galley,
        theme::ACCENT,
    );
}

/// Pipeline badge per catalog format (legacy `TopBar.tsx`).
fn format_badge(format: &str) -> String {
    match format {
        "raw" => "RAW · scene-linear".into(),
        "heif" | "hif" => "HDR PQ · scene-linear".into(),
        "hdr" | "exr" => "HDR · scene-linear".into(),
        "jpeg" => "JPEG · display-referred".into(),
        "png" => "PNG · display-referred".into(),
        other => format!("{} · scene-linear", other.to_uppercase()),
    }
}

/// Dismissible red strip under the top bar for the last UI-command dispatch error.
pub(crate) fn command_error_strip(
    root: &mut egui::Ui,
    command_error: &mut Option<(String, Instant)>,
) {
    let Some(message) = command_error.as_ref().map(|(message, _)| message.clone()) else {
        return;
    };
    let frame = egui::Frame::new()
        .fill(theme::REJECT.gamma_multiply(0.25))
        .inner_margin(Margin::symmetric(12, 5));
    egui::Panel::top(COMMAND_ERROR_ID)
        .frame(frame)
        .show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(message).color(theme::T1));
                if ui.small_button("Dismiss").clicked() {
                    *command_error = None;
                }
            });
        });
}

/// Centred explanation for the Duplicates tab.
pub(crate) fn duplicates_placeholder(root: &mut egui::Ui) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::STAGE))
        .show(root, |ui| {
            let rect = ui.max_rect();
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Duplicate review isn't in the native app yet.",
                FontId::proportional(13.0),
                theme::T2,
            );
        });
}
