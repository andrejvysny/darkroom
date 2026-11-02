//! Virtualised, day-grouped file list and the preview box.

use super::state::{is_dup, ImportUi, Row};
use crate::{theme, ThumbSource, ThumbState};
use core_app::import::{FileStatus, ImportFile, ImportView};
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};

const ROW_H: f32 = 24.0;
const CHECK_W: f32 = 30.0;

fn fmt_size(bytes: i64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else {
        format!("{} KB", ((bytes as f64 / 1024.0).round() as i64).max(1))
    }
}

fn badge(status: FileStatus) -> Option<&'static str> {
    match status {
        FileStatus::DuplicateLibrary => Some("in library"),
        FileStatus::DuplicateBatch => Some("duplicate"),
        _ => None,
    }
}

fn checkbox(painter: &egui::Painter, center: Pos2, on: bool, enabled: bool) {
    let rect = Rect::from_center_size(center, egui::vec2(14.0, 14.0));
    let line = if on { theme::ACCENT } else { theme::T3 };
    if on {
        painter.rect_filled(rect, 3, theme::ACCENT_DIM);
    }
    painter.rect_stroke(
        rect,
        3,
        Stroke::new(1.0, line.gamma_multiply(if enabled { 1.0 } else { 0.4 })),
        StrokeKind::Inside,
    );
    if on {
        let c = rect.center();
        let stroke = Stroke::new(1.6, theme::ACCENT);
        painter.line_segment(
            [c + egui::vec2(-3.5, 0.0), c + egui::vec2(-1.0, 3.0)],
            stroke,
        );
        painter.line_segment(
            [c + egui::vec2(-1.0, 3.0), c + egui::vec2(4.0, -3.5)],
            stroke,
        );
    }
}

fn header_row(ui: &mut Ui, day: &str, count: usize, checked: bool) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0, theme::ELEV);
    checkbox(
        &painter,
        Pos2::new(rect.left() + 15.0, rect.center().y),
        checked,
        true,
    );
    painter.text(
        Pos2::new(rect.left() + CHECK_W, rect.center().y),
        Align2::LEFT_CENTER,
        day,
        FontId::proportional(12.0),
        theme::T1,
    );
    painter.text(
        Pos2::new(rect.right() - 8.0, rect.center().y),
        Align2::RIGHT_CENTER,
        format!("{count} files"),
        FontId::proportional(11.0),
        theme::T3,
    );
    response.clicked()
}

enum RowAction {
    None,
    Toggle,
    Preview,
}

fn file_row(
    ui: &mut Ui,
    f: &ImportFile,
    selected: bool,
    enabled: bool,
    previewed: bool,
) -> RowAction {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let painter = ui.painter_at(rect);
    if previewed {
        painter.rect_filled(rect, 0, theme::ACCENT_DIM);
    } else if response.hovered() {
        painter.rect_filled(rect, 0, theme::HOVER);
    }
    checkbox(
        &painter,
        Pos2::new(rect.left() + 15.0, rect.center().y),
        selected,
        enabled,
    );
    let fade = if enabled { 1.0 } else { 0.45 };
    let mid = rect.center().y;
    let mut x = rect.right() - 8.0;
    let size = painter.text(
        Pos2::new(x, mid),
        Align2::RIGHT_CENTER,
        fmt_size(f.size_bytes),
        FontId::proportional(11.0),
        theme::T2,
    );
    x = size.left() - 10.0;
    let kind = painter.text(
        Pos2::new(x, mid),
        Align2::RIGHT_CENTER,
        f.kind.to_uppercase(),
        FontId::proportional(10.5),
        theme::T3,
    );
    x = kind.left() - 10.0;
    if let Some(text) = badge(f.status) {
        let galley =
            painter.layout_no_wrap(text.to_owned(), FontId::proportional(10.0), theme::STAR);
        let pill = Rect::from_min_size(
            Pos2::new(x - galley.size().x - 10.0, mid - 8.0),
            egui::vec2(galley.size().x + 10.0, 16.0),
        );
        painter.rect_stroke(
            pill,
            4,
            Stroke::new(1.0, theme::STAR.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
        painter.galley(
            Pos2::new(pill.left() + 5.0, mid - galley.size().y * 0.5),
            galley,
            theme::STAR,
        );
        x = pill.left() - 8.0;
    }
    // Painted last, elided to the space the right-hand columns leave, so long names never run
    // under the badge / kind / size.
    let left = rect.left() + CHECK_W;
    let mut job = egui::text::LayoutJob::simple_singleline(
        f.filename.clone(),
        FontId::monospace(11.5),
        theme::T1.gamma_multiply(fade),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((x - left).max(0.0));
    let galley = painter.layout_job(job);
    painter.galley(
        Pos2::new(left, mid - galley.size().y * 0.5),
        galley,
        theme::T1,
    );
    if !response.clicked() {
        return RowAction::None;
    }
    let on_box = response
        .interact_pointer_pos()
        .is_some_and(|p| p.x < rect.left() + CHECK_W);
    if on_box && enabled {
        RowAction::Toggle
    } else {
        RowAction::Preview
    }
}

/// Draws the visible slice of the grouped list; selection edits apply to `ui_state`.
pub(super) fn show(ui: &mut Ui, view: &ImportView, state: &mut ImportUi, height: f32) {
    let total = state.cache().map_or(0, |c| c.rows.len());
    if total == 0 {
        ui.add_space(20.0);
        ui.label(egui::RichText::new("No importable files here.").color(theme::T3));
        state.laid_out_rows = 0;
        return;
    }
    let mut toggles: Vec<(String, bool)> = Vec::new();
    let mut group_toggle: Option<(std::ops::Range<usize>, bool)> = None;
    let mut preview = None;
    ui.spacing_mut().item_spacing.y = 0.0;
    let out = egui::ScrollArea::vertical()
        .id_salt("import_files")
        .max_height(height)
        .auto_shrink([false, false])
        .show_rows(ui, ROW_H, total, |ui, range| {
            let n = range.len();
            for r in range {
                let Some(row) = state.cache().map(|c| c.rows[r].clone()) else {
                    break;
                };
                match row {
                    Row::Header { day, members } => {
                        let checked = state.group_checked(view, &members);
                        if header_row(ui, &day, members.len(), checked) {
                            group_toggle = Some((members, !checked));
                        }
                    }
                    Row::File(i) => {
                        let f = &view.files[i];
                        let enabled = !(state.skip_duplicates && is_dup(f.status));
                        let selected = state.is_selected(&f.path);
                        let previewed = state.preview.as_deref() == Some(f.path.as_str());
                        match file_row(ui, f, selected, enabled, previewed) {
                            RowAction::Toggle => toggles.push((f.path.clone(), !selected)),
                            RowAction::Preview => preview = Some(f.path.clone()),
                            RowAction::None => {}
                        }
                    }
                }
            }
            n
        });
    state.laid_out_rows = out.inner;
    for (path, on) in toggles {
        state.toggle_path(view, &path, on);
    }
    if let Some((members, on)) = group_toggle {
        state.set_group(view, members, on);
    }
    if preview.is_some() {
        state.preview = preview;
    }
}

/// Preview of the clicked source file (embedded thumbnail via the host's queue).
pub(super) fn preview_box(ui: &mut Ui, state: &ImportUi, thumbs: &mut dyn ThumbSource) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 170.0), Sense::hover());
    ui.painter()
        .rect_filled(rect, theme::RADIUS_SM, theme::STAGE);
    let Some(path) = state.preview.as_deref() else {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "Click a file to preview",
            FontId::proportional(12.0),
            theme::T3,
        );
        return;
    };
    match thumbs.file_preview(path) {
        ThumbState::Ready { texture, size } => {
            let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
            let scale = (rect.width() / w).min(rect.height() / h);
            let fit = Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
            ui.painter().image(
                texture,
                fit,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        ThumbState::Loading => {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Loading preview…",
                FontId::proportional(12.0),
                theme::T3,
            );
        }
        ThumbState::Failed => {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "No preview available",
                FontId::proportional(12.0),
                theme::T3,
            );
        }
    }
}
