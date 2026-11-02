//! Virtualised thumbnail grid. Only the grid rows intersecting the scroll viewport are laid out,
//! and [`ThumbSource::thumb`] is called only for those cells, in row-major order.

use super::filters::{any_filter, clear_filters};
use super::{cull_enabled, requery, select, LibraryUi, GRID_SCROLL_ID};
use crate::icons::{self, Icon};
use crate::widgets::pill_button;
use crate::widgets::{describe_selected, focus_ring, label_color, label_name, star_rating_at};
use crate::{theme, PhotoUi, Presence, ThumbSource, ThumbState};
use core_app::import::ImportRequest;
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
    photo: &mut PhotoUi,
    import_requests: &mut Vec<ImportRequest>,
    commands: &mut Vec<Command>,
) {
    lib.cell_rects.clear();
    if lib.loupe && snapshot.editor.selected.is_some() {
        loupe(root, snapshot, lib, photo);
        return;
    }
    lib.loupe_zoom.reset();
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::STAGE))
        .show(root, |ui| {
            let rows = &snapshot.library.rows;
            if rows.is_empty() {
                empty_state(ui, snapshot, lib, import_requests, commands);
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
                        cell(ui, snapshot, &rows[index], rect, thumbs, lib, commands);
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

/// Accessible name of a photo: filename plus rating, flag and colour label.
pub(crate) fn cell_name(row: &ImageRow, missing: bool) -> String {
    let mut name = row.filename.clone();
    if row.stars > 0 {
        let n = row.stars.min(5);
        name.push_str(&format!(", {n} star{}", if n == 1 { "" } else { "s" }));
    }
    match row.flag.as_str() {
        "pick" => name.push_str(", Pick"),
        "reject" => name.push_str(", Rejected"),
        _ => {}
    }
    if let Some(label) = row.color_label.as_deref().and_then(ColorLabel::from_row) {
        name.push_str(&format!(", {} label", label_name(label).to_lowercase()));
    }
    if missing {
        name.push_str(", missing");
    }
    name
}

fn cell(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    row: &ImageRow,
    rect: Rect,
    thumbs: &mut dyn ThumbSource,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    let id = Id::unique(("library_cell", row.id));
    let mut response = ui.interact(rect, id, Sense::click());
    let missing = thumbs.presence(row) == Presence::Missing;
    let fade = if row.flag == "reject" || missing {
        0.45
    } else {
        1.0
    };
    let primary = snapshot.editor.selected == Some(ImageId(row.id));
    let selected = primary || lib.selected.contains(&row.id);
    describe_selected(
        &response,
        egui::accesskit::Role::ListBoxOption,
        true,
        selected,
        &cell_name(row, missing),
    );
    focus_ring(ui, &response, rect.expand(3.0), 6);
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
    if missing {
        missing_pill(&painter, thumb_rect);
        response = response.on_hover_text(format!("Original file is missing: {}", row.path));
    }
    footer(ui, snapshot, row, rect, id, fade, commands);
    if selected {
        painter.rect_stroke(
            rect.expand(3.0),
            6,
            Stroke::new(
                1.5,
                if primary {
                    theme::ACCENT
                } else {
                    theme::ACCENT_LINE
                },
            ),
            StrokeKind::Inside,
        );
    }
    super::remove_dialog::context_menu(&response, snapshot, lib, row.id);
    if response.clicked() || response.double_clicked() {
        let modifiers = ui.input(|i| i.modifiers);
        select::click(snapshot, lib, row.id, modifiers, commands);
    }
    if response.double_clicked() {
        lib.loupe = true;
    }
}

/// Loupe: the Develop stage in the Library centre, with the same zoom gestures (wheel, pinch,
/// double-click / `Z` for 1:1). Esc or the toolbar toggle returns to the grid.
fn loupe(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    photo: &mut PhotoUi,
) {
    let typing = root.ctx().text_edit_focused() || lib.modal_open();
    crate::develop::stage::central(
        root,
        snapshot,
        photo,
        Some(&mut lib.loupe_zoom),
        typing,
        None,
    );
}

fn empty_state(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    import_requests: &mut Vec<ImportRequest>,
    commands: &mut Vec<Command>,
) {
    let query = &snapshot.library.query;
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // Search is not a filter dimension (`any_filter` keeps it out so "Clear filters" leaves it),
    // but it narrows the result just the same: only an unnarrowed empty result is an empty
    // catalog.
    let filtered = any_filter(query) || query.detected_category.is_some();
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.4).max(24.0));
        if !snapshot.catalog_ready {
            ui.label(RichText::new("No catalog open.").color(theme::T2));
        } else if !filtered && search.is_none() && snapshot.library.count == 0 {
            ui.label(
                RichText::new("Your library is empty")
                    .size(16.0)
                    .color(theme::T1),
            );
            ui.add_space(8.0);
            let add = pill_button(ui, Some(Icon::Upload), "Add photos…", true);
            lib.button_rects.push(("add_photos", add.rect));
            if add.clicked() {
                import_requests.push(ImportRequest::Open);
            }
        } else {
            let message = match (search, filtered) {
                (Some(text), false) => format!("No photos match \u{201c}{text}\u{201d}."),
                (Some(text), true) => {
                    format!("No photos match \u{201c}{text}\u{201d} with the current filters.")
                }
                (None, _) => "No photos match the current filters.".to_owned(),
            };
            ui.label(RichText::new(message).color(theme::T2));
            ui.add_space(8.0);
            // Stacked, not side by side: `vertical_centered` centres each widget on its own row.
            if search.is_some() {
                let clear = pill_button(ui, None, "Clear search", true);
                lib.button_rects.push(("clear_search", clear.rect));
                if clear.clicked() {
                    lib.search.clear();
                    lib.search_edited_at = None;
                    commands.push(requery(snapshot, |q| q.search = None));
                }
            }
            if filtered {
                ui.add_space(4.0);
                let clear = pill_button(ui, None, "Clear filters", true);
                lib.button_rects.push(("clear_filters", clear.rect));
                if clear.clicked() {
                    commands.push(requery(snapshot, clear_filters));
                }
            }
        }
    });
}

/// Opaque on purpose: the cell is faded, the warning must not be.
fn missing_pill(painter: &egui::Painter, thumb: Rect) {
    let pill = Rect::from_min_size(
        Pos2::new(thumb.left() + 5.0, thumb.bottom() - 19.0),
        egui::vec2(46.0, 14.0),
    );
    painter.rect_filled(pill, 7, theme::REJECT);
    painter.text(
        pill.center(),
        Align2::CENTER_CENTER,
        "Missing",
        FontId::proportional(9.5),
        theme::STAGE,
    );
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
