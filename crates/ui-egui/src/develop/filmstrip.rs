//! Bottom filmstrip: the selected photo's star rating, then a horizontal virtualised strip of the
//! library rows. [`ThumbSource::thumb`] is called only for the cells in the scroll viewport.

use super::DevelopUi;
use crate::library::{cull_enabled, selected_row};
use crate::widgets::star_rating;
use crate::{theme, ThumbSource, ThumbState};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, CullChange};
use core_model::identity::ImageId;
use egui::{Align, Align2, Color32, FontId, Id, Margin, Pos2, Rect, Sense, Stroke, StrokeKind};

pub const FILMSTRIP_ID: &str = "develop_filmstrip";
const HEIGHT: f32 = 72.0;
const SLOT_W: f32 = 84.0;
const SLOT_H: f32 = 56.0;
const GAP: f32 = 6.0;

pub(super) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    thumbs: &mut dyn ThumbSource,
    commands: &mut Vec<Command>,
) {
    st.film_rects.clear();
    let frame = egui::Frame::new()
        .fill(theme::APP)
        .stroke(Stroke::new(1.0, theme::LINE))
        .inner_margin(Margin {
            left: 14,
            right: 8,
            top: 0,
            bottom: 0,
        });
    egui::Panel::bottom(FILMSTRIP_ID)
        .exact_size(HEIGHT)
        .resizable(false)
        .frame(frame)
        .show(root, |ui| {
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 14.0;
                rating(ui, snapshot, commands);
                strip(ui, snapshot, st, thumbs, commands);
            });
        });
}

fn rating(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot, commands: &mut Vec<Command>) {
    let row = selected_row(snapshot);
    let stars = row.map_or(0, |r| u8::try_from(r.stars.clamp(0, 5)).unwrap_or(0));
    let interactive = row.is_some() && cull_enabled(snapshot);
    if let Some(n) = star_rating(ui, stars, 13.0, interactive) {
        if let Some(row) = row {
            commands.push(Command::SetCull {
                image: ImageId(row.id),
                change: CullChange::Rating(n),
            });
        }
    }
}

fn strip(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    thumbs: &mut dyn ThumbSource,
    commands: &mut Vec<Command>,
) {
    let rows = &snapshot.library.rows;
    if rows.is_empty() {
        ui.label(egui::RichText::new("No photos").size(12.0).color(theme::T3));
        return;
    }
    let pitch = SLOT_W + GAP;
    egui::ScrollArea::horizontal()
        .id_salt("develop_filmstrip_scroll")
        .auto_shrink([false, false])
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show_viewport(ui, |ui, viewport| {
            let height = ui.available_height().max(SLOT_H);
            let total = rows.len() as f32 * pitch - GAP;
            let (area, _) = ui.allocate_exact_size(egui::vec2(total, height), Sense::hover());
            let slot = |index: usize| {
                Rect::from_min_size(
                    Pos2::new(
                        area.left() + index as f32 * pitch,
                        area.center().y - SLOT_H * 0.5,
                    ),
                    egui::vec2(SLOT_W, SLOT_H),
                )
            };
            let selected = snapshot.editor.selected;
            if selected != st.film_scrolled_to {
                st.film_scrolled_to = selected;
                if let Some(index) =
                    selected.and_then(|id| rows.iter().position(|row| row.id == id.0))
                {
                    ui.scroll_to_rect(slot(index).expand(GAP), Some(Align::Center));
                }
            }
            let first = ((viewport.min.x / pitch).floor().max(0.0) as usize).min(rows.len());
            let last = ((viewport.max.x / pitch).ceil().max(0.0) as usize).min(rows.len());
            for index in first..last {
                let rect = slot(index);
                let row = &rows[index];
                let active = selected == Some(ImageId(row.id));
                if cell(ui, row, rect, active, thumbs) {
                    commands.push(Command::SelectImage(Some(ImageId(row.id))));
                }
                st.film_rects.push((row.id, rect));
            }
        });
}

/// Paints one cell; true when it was clicked.
fn cell(
    ui: &mut egui::Ui,
    row: &core_model::catalog::ImageRow,
    rect: Rect,
    active: bool,
    thumbs: &mut dyn ThumbSource,
) -> bool {
    let response = ui.interact(
        rect,
        Id::unique(("develop_film_cell", row.id)),
        Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 4, Color32::from_rgb(0x1a, 0x1a, 0x1a));
    let fade = if row.flag == "reject" { 0.45 } else { 1.0 };
    match thumbs.thumb(row) {
        ThumbState::Ready { texture, size } => {
            let (w, h) = (size[0].max(1) as f32, size[1].max(1) as f32);
            let scale = (rect.width() / w).min(rect.height() / h);
            let fit = Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
            let uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
            let tint = Color32::WHITE.gamma_multiply(fade * if active { 1.0 } else { 0.82 });
            painter.image(texture, fit, uv, tint);
        }
        ThumbState::Loading => {}
        ThumbState::Failed => {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "!",
                FontId::proportional(16.0),
                theme::T3,
            );
        }
    }
    let (width, color) = if active {
        (2.0, theme::ACCENT)
    } else if response.hovered() {
        (1.0, theme::LINE2)
    } else {
        (1.0, theme::LINE)
    };
    painter.rect_stroke(rect, 4, Stroke::new(width, color), StrokeKind::Inside);
    response.on_hover_text(&row.filename).clicked()
}
