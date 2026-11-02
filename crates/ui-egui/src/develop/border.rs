//! Border module (colour, size, aspect) and the stage's live border preview geometry. The border
//! itself is a CPU post-composite in the export path; the stage only paints the margin colour
//! around a smaller render of the photo, using the same `compute_frame` the legacy preview does.

use super::params::ControlKey as K;
use super::slider::{develop_slider, Env};
use super::DevelopUi;
use crate::theme;
use crate::widgets::pill_toggle;
use core_model::geometry::{compute_frame, FrameSettings};
use core_model::Border;
use egui::{Color32, Pos2, Rect, Ui};

/// Breathing room between the bordered frame and the stage edge, in points.
const STAGE_PAD: f32 = 24.0;

/// Legacy aspect choices: label, numerator, denominator (0 = keep the photo's aspect).
const ASPECTS: [(&str, f32, f32); 8] = [
    ("Original", 0.0, 0.0),
    ("1:1", 1.0, 1.0),
    ("4:5", 4.0, 5.0),
    ("5:4", 5.0, 4.0),
    ("3:2", 3.0, 2.0),
    ("2:3", 2.0, 3.0),
    ("16:9", 16.0, 9.0),
    ("9:16", 9.0, 16.0),
];

const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const BLACK: [f32; 3] = [0.0, 0.0, 0.0];

/// Where the bordered frame and the photo inside it land on the stage, in points.
pub(crate) struct BorderPreview {
    pub outer: Rect,
    pub inner: Rect,
}

/// The preview layout for a cropped frame of `frame_px` inside `stage`: the whole bordered frame
/// scaled to fit (minus a pad), centred, and the photo's rect within it. `None` when the border
/// is inactive or the frame is degenerate.
pub(crate) fn preview(stage: Rect, frame_px: (u32, u32), border: &Border) -> Option<BorderPreview> {
    if !border.is_active() || frame_px.0 == 0 || frame_px.1 == 0 {
        return None;
    }
    let (w, h) = (f64::from(frame_px.0), f64::from(frame_px.1));
    let frame = compute_frame(w, h, &FrameSettings::from(border));
    if !(frame.ow > 0.0 && frame.oh > 0.0) {
        return None;
    }
    let avail = egui::vec2(
        (stage.width() - 2.0 * STAGE_PAD).max(1.0),
        (stage.height() - 2.0 * STAGE_PAD).max(1.0),
    );
    let scale = (f64::from(avail.x) / frame.ow).min(f64::from(avail.y) / frame.oh) as f32;
    let outer = Rect::from_center_size(
        stage.center(),
        egui::vec2(frame.ow as f32 * scale, frame.oh as f32 * scale),
    );
    let inner = Rect::from_min_size(
        Pos2::new(
            outer.left() + frame.off_x as f32 * scale,
            outer.top() + frame.off_y as f32 * scale,
        ),
        egui::vec2(w as f32 * scale, h as f32 * scale),
    );
    Some(BorderPreview { outer, inner })
}

/// The border colour as a paint colour (sRGB 0..1 channels).
pub(crate) fn paint_color(border: &Border) -> Color32 {
    let [r, g, b, _] = border.color_rgba8();
    Color32::from_rgb(r, g, b)
}

pub(super) fn panel(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    let border = env.params.map(|p| p.border).unwrap_or_default();
    let can_edit = env.enabled && env.params.is_some();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        ui.label(egui::RichText::new("Color").size(12.0).color(theme::T3));
        for (label, color) in [("White", WHITE), ("Black", BLACK)] {
            if pill_toggle(ui, None, label, can_edit, border.color == color).clicked() {
                env.edit(|p| p.border.color = color);
            }
        }
        custom_color(ui, env, &border, can_edit);
    });
    develop_slider(ui, "Size", K::BorderSize, env, st);
    ui.label(
        egui::RichText::new("Aspect ratio")
            .size(12.0)
            .color(theme::T3),
    );
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (label, w, h) in ASPECTS {
            let selected = (border.aspect_w, border.aspect_h) == (w, h);
            if pill_toggle(ui, None, label, can_edit, selected).clicked() {
                env.edit(|p| {
                    p.border.aspect_w = w;
                    p.border.aspect_h = h;
                });
            }
        }
    });
}

/// "Custom" pill (selected for any colour other than white/black) that opens the colour picker;
/// every change is one discrete edit.
fn custom_color(ui: &mut Ui, env: &mut Env<'_>, border: &Border, can_edit: bool) {
    let custom = border.color != WHITE && border.color != BLACK;
    let pill = pill_toggle(ui, None, "Custom", can_edit, custom);
    if !can_edit {
        return;
    }
    egui::Popup::from_toggle_button_response(&pill)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            let [r, g, b, _] = border.color_rgba8();
            let mut color = Color32::from_rgb(r, g, b);
            if egui::color_picker::color_picker_color32(
                ui,
                &mut color,
                egui::color_picker::Alpha::Opaque,
            ) {
                let next = [
                    f32::from(color.r()) / 255.0,
                    f32::from(color.g()) / 255.0,
                    f32::from(color.b()) / 255.0,
                ];
                env.edit(|p| p.border.color = next);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage() -> Rect {
        Rect::from_min_size(Pos2::ZERO, egui::vec2(848.0, 673.0))
    }

    #[test]
    fn an_inactive_border_has_no_preview() {
        assert!(preview(stage(), (6000, 4000), &Border::default()).is_none());
    }

    #[test]
    fn a_margin_shrinks_the_photo_inside_a_centred_frame() {
        let border = Border {
            size: 5.0,
            ..Default::default()
        };
        let p = preview(stage(), (6000, 4000), &border).expect("active");
        assert!((p.outer.center() - stage().center()).length() < 0.5);
        assert!(p.inner.width() < p.outer.width() && p.inner.height() < p.outer.height());
        // 5% of the 6000 long edge = 300 px each side: the photo is 6000/6600 of the frame.
        let ratio = p.inner.width() / p.outer.width();
        assert!((ratio - 6000.0 / 6600.0).abs() < 1e-3, "{ratio}");
        assert!(stage().contains_rect(p.outer));
        assert!(p.outer.contains_rect(p.inner));
    }

    #[test]
    fn an_aspect_pads_the_photo_to_the_target_ratio() {
        let border = Border {
            aspect_w: 1.0,
            aspect_h: 1.0,
            ..Default::default()
        };
        let p = preview(stage(), (6000, 4000), &border).expect("active");
        assert!((p.outer.width() - p.outer.height()).abs() < 1.0);
        assert!((p.inner.width() / p.inner.height() - 1.5).abs() < 1e-3);
    }
}
