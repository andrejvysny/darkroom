//! Small custom-painted widgets shared by the shell and Library panels.

use crate::icons::{self, Icon};
use crate::theme;
use core_app::workflow::ColorLabel;
use egui::{Align2, Color32, FontId, Id, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui};

/// `2124` -> `"2,124"`.
pub fn fmt_count(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if value < 0 {
        out.insert(0, '-');
    }
    out
}

pub fn label_color(label: ColorLabel) -> Color32 {
    match label {
        ColorLabel::Red => theme::LAB_RED,
        ColorLabel::Yellow => theme::LAB_YELLOW,
        ColorLabel::Green => theme::LAB_GREEN,
        ColorLabel::Blue => theme::LAB_BLUE,
        ColorLabel::Purple => theme::LAB_PURPLE,
    }
}

pub const LABELS: [ColorLabel; 5] = [
    ColorLabel::Red,
    ColorLabel::Yellow,
    ColorLabel::Green,
    ColorLabel::Blue,
    ColorLabel::Purple,
];

fn text_width(ui: &Ui, text: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::WHITE)
        .size()
        .x
}

/// Segmented control; the selected segment is elevated. Returns the clicked index.
pub fn segmented(ui: &mut Ui, labels: &[&str], selected: usize) -> Option<usize> {
    let mut clicked = None;
    egui::Frame::new()
        .fill(theme::ELEV)
        .stroke(Stroke::new(1.0, theme::LINE))
        .corner_radius(theme::RADIUS_SM)
        .inner_margin(2)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal(|ui| {
                for (i, label) in labels.iter().enumerate() {
                    let width = text_width(ui, label, 12.5) + 26.0;
                    let (rect, response) =
                        ui.allocate_exact_size(egui::vec2(width, 24.0), Sense::click());
                    let on = i == selected;
                    if on || response.hovered() {
                        let fill = if on {
                            theme::HOVER
                        } else {
                            theme::HOVER.gamma_multiply(0.5)
                        };
                        ui.painter().rect_filled(rect, 4, fill);
                    }
                    let color = if on { theme::T1 } else { theme::T2 };
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        label,
                        FontId::proportional(12.5),
                        color,
                    );
                    if response.clicked() {
                        clicked = Some(i);
                    }
                }
            });
        });
    clicked
}

/// Outlined pill button with an optional leading icon.
pub fn pill_button(ui: &mut Ui, icon: Option<Icon>, label: &str, enabled: bool) -> Response {
    let font = FontId::proportional(12.5);
    let icon_w = if icon.is_some() { 20.0 } else { 0.0 };
    let width = text_width(ui, label, 12.5) + icon_w + 20.0;
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 26.0), sense);
    let hot = enabled && response.hovered();
    if hot {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_SM, theme::HOVER);
    }
    ui.painter().rect_stroke(
        rect,
        theme::RADIUS_SM,
        Stroke::new(1.0, theme::LINE2),
        StrokeKind::Inside,
    );
    let color = if !enabled {
        theme::T3
    } else if hot {
        theme::T1
    } else {
        theme::T2
    };
    let mut x = rect.left() + 10.0;
    if let Some(icon) = icon {
        let r = Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), egui::vec2(14.0, 14.0));
        icons::paint(ui.painter(), r, icon, color);
        x += 20.0;
    }
    ui.painter().text(
        Pos2::new(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    response
}

/// Small toggle chip ("Picks", "Rejects").
pub fn chip(ui: &mut Ui, label: &str, active: bool) -> Response {
    let width = text_width(ui, label, 11.5) + 18.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 22.0), Sense::click());
    let (fill, line, text) = if active {
        (theme::ACCENT_DIM, theme::ACCENT_LINE, theme::T1)
    } else if response.hovered() {
        (theme::HOVER, theme::LINE2, theme::T1)
    } else {
        (Color32::TRANSPARENT, theme::LINE2, theme::T2)
    };
    ui.painter().rect_filled(rect, theme::RADIUS_SM, fill);
    ui.painter().rect_stroke(
        rect,
        theme::RADIUS_SM,
        Stroke::new(1.0, line),
        StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::proportional(11.5),
        text,
    );
    response
}

/// Nav list row: icon + label + right-aligned mono count; selected = elevated bg + accent bar.
pub fn nav_row(
    ui: &mut Ui,
    icon: Option<Icon>,
    label: &str,
    count: Option<i64>,
    selected: bool,
    indent: f32,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), Sense::click());
    if selected {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_SM, theme::ELEV);
        let bar = Rect::from_min_size(
            rect.min + egui::vec2(0.0, 4.0),
            egui::vec2(2.0, rect.height() - 8.0),
        );
        ui.painter().rect_filled(bar, 1, theme::ACCENT);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, theme::RADIUS_SM, theme::HOVER.gamma_multiply(0.6));
    }
    let color = if selected { theme::T1 } else { theme::T2 };
    let mut x = rect.left() + 10.0 + indent;
    if let Some(icon) = icon {
        let r = Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), egui::vec2(14.0, 14.0));
        icons::paint(ui.painter(), r, icon, color);
        x += 22.0;
    }
    ui.painter().text(
        Pos2::new(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(12.5),
        color,
    );
    if let Some(count) = count {
        ui.painter().text(
            Pos2::new(rect.right() - 8.0, rect.center().y),
            Align2::RIGHT_CENTER,
            fmt_count(count),
            FontId::monospace(11.0),
            theme::T3,
        );
    }
    response
}

/// Non-interactive italic hint row.
pub fn placeholder_row(ui: &mut Ui, text: &str) {
    ui.horizontal(|ui| {
        ui.add_space(10.0);
        ui.label(
            egui::RichText::new(text)
                .italics()
                .size(12.0)
                .color(theme::T3),
        );
    });
}

/// Five stars at `origin`; returns the clicked rating (clicking the current value gives 0).
/// `fade` multiplies the colours (rejected cells).
pub fn star_rating_at(
    ui: &mut Ui,
    origin: Pos2,
    id: Id,
    value: u8,
    size: f32,
    interactive: bool,
    fade: f32,
) -> Option<u8> {
    let gap = 2.0;
    let mut clicked = None;
    for n in 1..=5u8 {
        let x = origin.x + f32::from(n - 1) * (size + gap);
        let rect = Rect::from_min_size(Pos2::new(x, origin.y), egui::vec2(size, size));
        let (icon, color) = if n <= value {
            (Icon::StarFilled, theme::STAR)
        } else {
            (Icon::Star, theme::T3)
        };
        if interactive {
            let response = ui.interact(rect.expand(1.0), id.with(n), Sense::click());
            if response.clicked() {
                clicked = Some(if n == value { 0 } else { n });
            }
        }
        icons::paint(ui.painter(), rect, icon, color.gamma_multiply(fade));
    }
    clicked
}

/// Inline star rating (allocates its own row).
pub fn star_rating(ui: &mut Ui, value: u8, size: f32, interactive: bool) -> Option<u8> {
    let width = 5.0 * size + 4.0 * 2.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size.max(16.0)), Sense::hover());
    let origin = Pos2::new(rect.left(), rect.center().y - size * 0.5);
    let id = ui
        .scope_id()
        .with(("stars", rect.min.x as i32, rect.min.y as i32));
    star_rating_at(ui, origin, id, value, size, interactive, 1.0)
}

/// Five colour dots. `Some(None)` = clicked the selected one (clear); `Some(Some(l))` = set.
pub fn color_dots(
    ui: &mut Ui,
    selected: Option<ColorLabel>,
    size: f32,
) -> Option<Option<ColorLabel>> {
    let mut result = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for label in LABELS {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(size + 4.0, size + 4.0), Sense::click());
            let color = label_color(label);
            ui.painter().circle_filled(rect.center(), size * 0.5, color);
            if selected == Some(label) {
                ui.painter().circle_stroke(
                    rect.center(),
                    size * 0.5 + 2.0,
                    Stroke::new(1.3, theme::T1),
                );
            } else if response.hovered() {
                ui.painter().circle_stroke(
                    rect.center(),
                    size * 0.5 + 2.0,
                    Stroke::new(1.0, theme::LINE2),
                );
            }
            if response.on_hover_text(label.as_str()).clicked() {
                result = Some(if selected == Some(label) {
                    None
                } else {
                    Some(label)
                });
            }
        }
    });
    result
}
