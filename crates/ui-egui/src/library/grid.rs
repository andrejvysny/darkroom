//! Virtualised thumbnail grid. Only the grid rows intersecting the scroll viewport are laid out,
//! and [`ThumbSource::thumb`] is called only for those cells, in row-major order.

use super::{cull_enabled, LibraryUi, GRID_SCROLL_ID};
use crate::icons::{self, Icon};
use crate::widgets::{label_color, star_rating_at};
use crate::{theme, Mode, ThumbSource, ThumbState};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{ColorLabel, Command, CullChange};
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, RichText, Sense, Stroke, StrokeKind};

const PAD: f32 = 12.0;
const GAP: f32 = 8.0;
const FOOTER: f32 = 18.0;
const STAR_SIZE: f32 = 9.0;

struct Metrics {
    cols: usize,
    cell: f32,
    row_h: f32,
}

impl Metrics {
    fn new(width: f32, cell_size: f32) -> Self {
        let inner = (width - 2.0 * PAD).max(1.0);
        let cols = (((inner + GAP) / (cell_size + GAP)).floor() as usize).max(1);
        let cell = ((inner - GAP * (cols - 1) as f32) / cols as f32).max(24.0);
        Self {
            cols,
            cell,
            row_h: cell + FOOTER + GAP,
        }
    }

    fn cell_rect(&self, origin: Pos2, index: usize) -> Rect {
        let (r, c) = (index / self.cols, index % self.cols);
        let min = origin
            + egui::vec2(
                PAD + c as f32 * (self.cell + GAP),
                PAD + r as f32 * self.row_h,
            );
        Rect::from_min_size(min, egui::vec2(self.cell, self.cell + FOOTER))
    }
}

pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    thumbs: &mut dyn ThumbSource,
    lib: &mut LibraryUi,
    mode: &mut Mode,
    commands: &mut Vec<Command>,
) {
    lib.cell_rects.clear();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::STAGE))
        .show(root, |ui| {
            let rows = &snapshot.library.rows;
            if rows.is_empty() {
                let text = if snapshot.catalog_ready {
                    "No photos match the current filters."
                } else {
                    "No catalog open."
                };
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(text).color(theme::T2));
                });
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt(GRID_SCROLL_ID)
                .auto_shrink([false, false])
                .show_viewport(ui, |ui, viewport| {
                    let width = ui.available_width();
                    let m = Metrics::new(width, lib.cell_size);
                    lib.columns = m.cols;
                    let n_rows = rows.len().div_ceil(m.cols);
                    let total = 2.0 * PAD + n_rows as f32 * m.row_h - GAP;
                    let (area, _) =
                        ui.allocate_exact_size(egui::vec2(width, total), Sense::hover());
                    scroll_to_selection(ui, snapshot, lib, &m, area.min);
                    let first =
                        (((viewport.min.y - PAD) / m.row_h).floor().max(0.0) as usize).min(n_rows);
                    let last =
                        (((viewport.max.y - PAD) / m.row_h).ceil().max(0.0) as usize).min(n_rows);
                    let range = (first * m.cols)..(last * m.cols).min(rows.len());
                    for index in range {
                        let rect = m.cell_rect(area.min, index);
                        cell(ui, snapshot, &rows[index], rect, thumbs, mode, commands);
                        lib.cell_rects.push((rows[index].id, rect));
                    }
                });
        });
}

/// Keeps a keyboard/command-driven selection visible; does nothing while it is unchanged.
fn scroll_to_selection(
    ui: &egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    m: &Metrics,
    origin: Pos2,
) {
    let selected = snapshot.editor.selected;
    if selected == lib.scrolled_to {
        return;
    }
    lib.scrolled_to = selected;
    let Some(id) = selected else { return };
    if let Some(index) = snapshot.library.rows.iter().position(|row| row.id == id.0) {
        ui.scroll_to_rect(m.cell_rect(origin, index).expand(GAP), None);
    }
}

fn cell(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    row: &ImageRow,
    rect: Rect,
    thumbs: &mut dyn ThumbSource,
    mode: &mut Mode,
    commands: &mut Vec<Command>,
) {
    let id = Id::unique(("library_cell", row.id));
    let mut response = ui.interact(rect, id, Sense::click());
    let fade = if row.flag == "reject" { 0.45 } else { 1.0 };
    let selected = snapshot.editor.selected == Some(ImageId(row.id));
    let painter = ui.painter().clone();
    if response.hovered() {
        painter.rect_filled(rect.expand(3.0), 6, theme::HOVER.gamma_multiply(fade));
    }
    let thumb_rect = Rect::from_min_size(rect.min, egui::vec2(rect.width(), rect.width()));
    painter.rect_filled(thumb_rect, 4, theme::STAGE);
    let state = thumbs.thumb(row);
    match state {
        ThumbState::Ready { texture, size } => {
            let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
            let scale = (thumb_rect.width() / w).min(thumb_rect.height() / h);
            let fit = Rect::from_center_size(thumb_rect.center(), egui::vec2(w * scale, h * scale));
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            painter.image(texture, fit, uv, Color32::WHITE.gamma_multiply(fade));
        }
        ThumbState::Loading => {
            painter.rect_filled(thumb_rect, 4, theme::ELEV.gamma_multiply(fade));
        }
        ThumbState::Failed => {
            painter.text(
                thumb_rect.center(),
                Align2::CENTER_CENTER,
                "!",
                FontId::proportional(20.0),
                theme::T3,
            );
            response = response.on_hover_text("Thumbnail unavailable");
        }
    }
    badges(&painter, row, thumb_rect, fade);
    footer(ui, snapshot, row, rect, id, fade, commands);
    if selected {
        painter.rect_stroke(
            rect.expand(3.0),
            6,
            Stroke::new(1.5, theme::ACCENT),
            StrokeKind::Inside,
        );
    }
    if response.clicked() || response.double_clicked() {
        commands.push(Command::SelectImage(Some(ImageId(row.id))));
    }
    if response.double_clicked() {
        *mode = Mode::Develop;
    }
}

fn badges(painter: &egui::Painter, row: &ImageRow, thumb: Rect, fade: f32) {
    if row.flag == "pick" {
        let r = Rect::from_min_size(thumb.min + egui::vec2(5.0, 5.0), egui::vec2(13.0, 13.0));
        icons::paint(painter, r, Icon::Flag, theme::PICK.gamma_multiply(fade));
    }
    if let Some(label) = row.color_label.as_deref().and_then(ColorLabel::from_row) {
        let center = Pos2::new(thumb.right() - 8.0, thumb.top() + 8.0);
        painter.circle_filled(center, 3.0, label_color(label).gamma_multiply(fade));
    }
    if row.paired_count > 0 {
        let text = format!("+{}", row.paired_count);
        let badge = Rect::from_min_size(
            Pos2::new(thumb.right() - 26.0, thumb.bottom() - 17.0),
            egui::vec2(22.0, 13.0),
        );
        painter.rect_filled(badge, 3, theme::ELEV.gamma_multiply(0.9 * fade));
        painter.text(
            badge.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(9.0),
            theme::T2,
        );
    }
}

fn footer(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    row: &ImageRow,
    rect: Rect,
    id: Id,
    fade: f32,
    commands: &mut Vec<Command>,
) {
    let mid = rect.top() + rect.width() + FOOTER * 0.5;
    let stars = u8::try_from(row.stars.clamp(0, 5)).unwrap_or(0);
    let origin = Pos2::new(rect.left() + 2.0, mid - STAR_SIZE * 0.5);
    let clicked = star_rating_at(
        ui,
        origin,
        id.with("stars"),
        stars,
        STAR_SIZE,
        cull_enabled(snapshot),
        fade,
    );
    if let Some(n) = clicked {
        commands.push(Command::SetCull {
            image: ImageId(row.id),
            change: CullChange::Rating(n),
        });
    }
    let left = origin.x + 5.0 * STAR_SIZE + 4.0 * 2.0 + 6.0;
    if rect.right() - 2.0 > left + 8.0 {
        let label = Rect::from_min_max(
            Pos2::new(left, mid - 7.0),
            Pos2::new(rect.right() - 2.0, mid + 7.0),
        );
        let text = RichText::new(&row.filename)
            .monospace()
            .size(10.5)
            .color(theme::T2.gamma_multiply(fade));
        ui.put(label, egui::Label::new(text).truncate().selectable(false));
    }
}
