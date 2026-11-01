//! Centre of Develop: the presented frame centred on the dark stage with a soft shadow and the
//! info footer. The render plan itself is `photo::photo_plan`, unchanged; this only paints.

use crate::photo::photo_plan;
use crate::{theme, PhotoUi};
use core_app::state::ApplicationSnapshot;
use egui::{Align2, Color32, FontId, Rect};

/// `"10%  •  6960 × 4640  •  sRGB"`; the zoom is the presented frame's pixel width over the
/// output image's pixel width, and is left out while nothing is presented.
fn footer_text(presented_w: Option<u32>, dims: (u32, u32)) -> String {
    let size = format!("{} × {}  •  sRGB", dims.0, dims.1);
    match presented_w {
        Some(w) => {
            let pct = (f64::from(w) / f64::from(dims.0.max(1)) * 100.0).round();
            format!("{pct:.0}%  •  {size}")
        }
        None => size,
    }
}

/// Native output size of the selected image (catalog dims through the crop), if known.
fn output_dims(snapshot: &ApplicationSnapshot) -> Option<(u32, u32)> {
    let image = snapshot.editor.image.as_ref()?;
    let (w, h) = (
        u32::try_from(image.width?).ok()?,
        u32::try_from(image.height?).ok()?,
    );
    if w == 0 || h == 0 {
        return None;
    }
    Some(match snapshot.editor.document.params.as_ref() {
        Some(p) => p.crop.output_size(w, h),
        None => (w, h),
    })
}

pub(super) fn central(root: &mut egui::Ui, snapshot: &ApplicationSnapshot, photo: &mut PhotoUi) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::STAGE_DEV))
        .show(root, |ui| {
            // Save failures have their own banner with Discard/Dismiss (see `develop`).
            if let Some(error) = snapshot
                .error
                .as_ref()
                .filter(|error| !super::is_save_error(error))
            {
                ui.label(error.to_string());
            }
            // The panel rect in points is only known here; the plan carries it as physical pixels.
            let rect = ui.available_rect_before_wrap();
            photo.plan = if photo.message.is_some() {
                None
            } else {
                photo_plan(
                    snapshot,
                    rect,
                    ui.ctx().pixels_per_point(),
                    photo.device_epoch,
                )
            };
            if let Some(failure) = &photo.failure {
                ui.horizontal(|ui| {
                    ui.label(failure);
                    if ui.button("Retry").clicked() {
                        photo.retry = true;
                    }
                });
            }
            if let Some(message) = &photo.message {
                // A protected/unsupported/failed state names itself; never show a stale render
                // of another state under it.
                ui.label(message);
            } else if let Some((texture, size)) = photo.texture {
                // The frame was developed at the aspect-correct fitted size: draw it at exactly
                // that size (points = px / pixels_per_point) centred in the panel; the rest of
                // the panel stays the stage. The rect is never resized here — `ui.put` stretches
                // the texture to the given rect — so a stale frame from a larger previous panel
                // merely overflows, hidden by the panel's clip until the refit lands.
                let ppp = ui.ctx().pixels_per_point().max(f32::EPSILON);
                let size_points =
                    egui::vec2(size[0].max(1) as f32 / ppp, size[1].max(1) as f32 / ppp);
                let draw_rect = Rect::from_center_size(rect.center(), size_points);
                let shadow = egui::Shadow {
                    offset: [0, 6],
                    blur: 28,
                    spread: 0,
                    color: Color32::from_black_alpha(150),
                };
                ui.painter().add(shadow.as_shape(draw_rect, 0));
                let image =
                    egui::Image::from_texture(egui::load::SizedTexture::new(texture, size_points))
                        .fit_to_exact_size(size_points);
                ui.put(draw_rect, image);
                if photo.loading {
                    ui.spinner();
                }
            } else if photo.loading {
                ui.spinner();
                ui.label("Loading photo…");
            } else if snapshot.editor.image.is_none() {
                ui.label("Select a photo");
            }
            if let Some(dims) = output_dims(snapshot) {
                ui.painter().text(
                    rect.left_bottom() + egui::vec2(14.0, -12.0),
                    Align2::LEFT_BOTTOM,
                    footer_text(photo.texture.map(|(_, s)| s[0]), dims),
                    FontId::monospace(11.0),
                    theme::T3,
                );
            }
            // Claim the panel area for this frame's layout regardless of what was drawn in it.
            ui.allocate_rect(rect, egui::Sense::hover());
        });
}

#[cfg(test)]
mod tests {
    use super::footer_text;

    #[test]
    fn footer_shows_zoom_size_and_space() {
        assert_eq!(
            footer_text(Some(696), (6960, 4640)),
            "10%  •  6960 × 4640  •  sRGB"
        );
        assert_eq!(footer_text(None, (6960, 4640)), "6960 × 4640  •  sRGB");
    }
}
