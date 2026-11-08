//! Centre of Develop: the presented frame centred on the dark stage with a soft shadow and the
//! info footer. The render plan is `photo::photo_plan`; this only paints, but it fits the frame
//! into the panel itself (from the frame's aspect, never its texture size) so that a cheaper,
//! smaller frame occupies exactly the rect the settled one does.

use super::border;
use super::crop_tool::{self, CropState};
use super::stage_paint::{self, Win};
use super::zoom::ZoomUi;
use super::zoom_footer::{self, Zoomer};
use crate::photo::{fit_output, photo_plan, target_id, StageVariant};
use crate::{theme, PhotoUi};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use core_model::viewport::{derive_view, DerivedView, ViewState};
use core_model::Crop;
use egui::{Color32, CursorIcon, FontId, Rect};

/// What only the Develop stage has: the Before view, the crop tool and the commands the tool
/// emits. The Library loupe passes `None` and gets the plain edited picture.
pub(crate) struct DevelopStage<'a> {
    pub before: bool,
    pub crop: &'a mut CropState,
    pub commands: &'a mut Vec<Command>,
}

/// Native output size of the picture the stage shows (catalog dims through that variant's
/// crop), if known.
fn output_dims(snapshot: &ApplicationSnapshot, variant: StageVariant) -> Option<(u32, u32)> {
    let image = snapshot.editor.image.as_ref()?;
    let (w, h) = (
        u32::try_from(image.width?).ok()?,
        u32::try_from(image.height?).ok()?,
    );
    if w == 0 || h == 0 {
        return None;
    }
    Some(match (snapshot.editor.document.params.as_ref(), variant) {
        (Some(p), StageVariant::Edited) => p.crop.output_size(w, h),
        (Some(p), StageVariant::CropTool) => Crop {
            angle: p.crop.angle,
            rot90: p.crop.rot90,
            ..Crop::default()
        }
        .output_size(w, h),
        (Some(_), StageVariant::Before) | (None, _) => (w, h),
    })
}

/// The panel's physical-pixel size, computed exactly as `photo_plan` does, so the box the frame
/// is drawn into and the box it was rendered for are the same box.
fn panel_px(rect: Rect, ppp: f32) -> (u32, u32) {
    (
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    )
}

/// Where a frame of `tex_px` pixels lands in `rect`: the fit view — the largest size with the
/// frame's aspect that fits the panel — centred, in points; plus that width in physical px,
/// which is what the footer's zoom is measured against.
///
/// Deliberately derived from the panel and the frame's ASPECT, never from the texture's size:
/// an interactive frame is rendered at half the fitted size and must still fill exactly the
/// rect the settled frame occupies.
fn fitted_draw(rect: Rect, ppp: f32, tex_px: [u32; 2]) -> (Rect, u32) {
    let ppp = ppp.max(f32::EPSILON);
    let aspect = f64::from(tex_px[0].max(1)) / f64::from(tex_px[1].max(1));
    let (width, height) = fit_output(panel_px(rect, ppp), aspect);
    let size_points = egui::vec2(width as f32 / ppp, height as f32 / ppp);
    (Rect::from_center_size(rect.center(), size_points), width)
}

/// The stage's render geometry this frame, shared by the plan and the drawing so the box the
/// frame is rendered for and the box it is drawn into are the same box.
struct Geometry {
    /// The cropped frame's pixel dims (the thing being zoomed).
    frame_px: (u32, u32),
    /// The stage in physical px.
    panel_px: (u32, u32),
    /// `Some` while zoomed: the window the renderer renders and the whole image's on-screen size.
    derived: Option<DerivedView>,
}

/// Zoom needs real frame dims; without catalog dims the stage stays at fit.
fn geometry(
    snapshot: &ApplicationSnapshot,
    rect: Rect,
    ppp: f32,
    view: ViewState,
    variant: StageVariant,
) -> Option<Geometry> {
    let frame_px = output_dims(snapshot, variant)?;
    let panel_px = panel_px(rect, ppp);
    Some(Geometry {
        frame_px,
        panel_px,
        derived: (!view.is_fit()).then(|| derive_view(view, frame_px, panel_px)),
    })
}

/// What the stage drew this frame.
struct Drawn {
    /// Where the presented frame was painted, in points: the fitted rect at fit, the visible
    /// image area when zoomed — also the rect the zoom gestures anchor on.
    rect: Rect,
}

/// Where the whole image lands when zoomed, clipped to the stage: the box the window frame is a
/// 1:1 picture of (`out_px` is exactly this intersect's pixel size).
fn visible_rect(panel: Rect, derived: &DerivedView, ppp: f32) -> Rect {
    let ppp = ppp.max(f32::EPSILON);
    let image = Rect::from_center_size(
        panel.center(),
        egui::vec2(
            (derived.image_px[0].max(1.0) as f32) / ppp,
            (derived.image_px[1].max(1.0) as f32) / ppp,
        ),
    );
    panel.intersect(image)
}

/// The current view's window in crop-local UV, or `None` at fit (the whole frame).
fn view_window(derived: &DerivedView) -> Win {
    (
        [derived.origin[0] as f32, derived.origin[1] as f32],
        [derived.size[0] as f32, derived.size[1] as f32],
    )
}

/// What the stage holds this frame: the retry banner, then either a state message (which
/// replaces the image — a protected/unsupported/failed document must never be silently shown
/// as a render) or the composed picture: a compatible cached placeholder under the live frame,
/// each re-projected into the current view. `None` when nothing of the photo was drawn (and so
/// there is nothing to zoom into).
fn stage_content(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    photo: &mut PhotoUi,
    rect: Rect,
    geometry: Option<&Geometry>,
    variant: StageVariant,
    interacting: bool,
) -> Option<Drawn> {
    if let Some(failure) = &photo.failure {
        ui.horizontal(|ui| {
            ui.label(failure);
            let retry = ui.button("Retry");
            if retry.clicked() {
                photo.retry = true;
            }
        });
    }
    if let Some(message) = &photo.message {
        ui.label(message);
        return None;
    }
    // A placeholder is painted only when it is a picture of what is wanted now; a live frame is
    // always of the selected photo (the host retires it on any subject change).
    let target = target_id(snapshot, variant);
    let backdrop = photo
        .placeholder
        .filter(|pic| target.is_some_and(|target| pic.id.compatible(&target)));
    let live = photo.texture;
    let Some(base) = live.or(backdrop) else {
        if photo.loading {
            ui.spinner();
            ui.label("Loading photo…");
        } else if snapshot.editor.image.is_none() {
            ui.label("Select a photo");
        }
        return None;
    };
    let ppp = ui.ctx().pixels_per_point();
    let derived = geometry.and_then(|geometry| geometry.derived);
    let draw_rect = match derived {
        Some(derived) => visible_rect(rect, &derived, ppp),
        None => {
            // A window picture's own aspect is not the frame's: fit by the cropped frame's dims
            // when the document defines them.
            let fit_px = match geometry {
                Some(geo) if base.window.is_some() && snapshot.editor.document.params.is_some() => {
                    [geo.frame_px.0, geo.frame_px.1]
                }
                _ => base.px,
            };
            fitted_draw(rect, ppp, fit_px).0
        }
    };
    let window = derived.as_ref().map(view_window);
    let alpha = stage_paint::live_alpha(
        ui.ctx(),
        target.map(|target| target.image),
        live.map(|pic| pic.texture),
        live.is_none() && backdrop.is_some(),
        interacting,
    );
    stage_paint::paint(
        ui,
        draw_rect,
        backdrop.as_ref(),
        live.as_ref(),
        window,
        alpha,
    );
    stage_paint::activity_dot(ui, rect, photo.loading);
    Some(Drawn { rect: draw_rect })
}

/// "BEFORE" tag over the stage while the unedited picture is shown.
fn paint_before_badge(ui: &egui::Ui, stage: Rect) {
    let galley =
        ui.painter()
            .layout_no_wrap("BEFORE".to_owned(), FontId::proportional(11.0), theme::T1);
    let badge = Rect::from_min_size(
        stage.left_top() + egui::vec2(14.0, 14.0),
        galley.size() + egui::vec2(16.0, 8.0),
    );
    ui.painter()
        .rect_filled(badge, theme::RADIUS_SM, Color32::from_black_alpha(170));
    ui.painter()
        .galley(badge.min + egui::vec2(8.0, 4.0), galley, theme::T1);
}

/// The stage's pointer target for zoom/pan, registered BEFORE anything is painted: the stage
/// claims clicks and drags, but a real widget painted later (the retry banner's buttons) stays
/// on top of it and keeps its clicks.
fn stage_response(ui: &egui::Ui, rect: Rect) -> egui::Response {
    ui.interact(
        rect,
        egui::Id::unique("develop_stage_zoom"),
        egui::Sense::click_and_drag(),
    )
}

/// The stage: the presented frame on the dark background, the zoom/pan gestures and the footer.
/// `zoom` is the Develop stage's view state — `None` for the Library loupe, which shows the same
/// frame always at fit and claims no pointer input.
pub(crate) fn central(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    photo: &mut PhotoUi,
    mut zoom: Option<&mut ZoomUi>,
    typing: bool,
    mut develop: Option<DevelopStage<'_>>,
) {
    egui::CentralPanel::default()
        .frame(egui::Frame::new().fill(theme::STAGE_DEV))
        .show(root, |ui| {
            if let Some(zoom) = zoom.as_mut() {
                zoom.sync(snapshot);
            }
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
            let zoomable = zoom.is_some();
            let stage = zoomable.then(|| stage_response(ui, rect));
            if let Some(stage) = &stage {
                let name = snapshot.editor.image.as_ref().map_or_else(
                    || "Photo".to_owned(),
                    |row| format!("{} photo", row.filename),
                );
                stage.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::accesskit::Role::Image, true, &name)
                });
            }
            let ppp = ui.ctx().pixels_per_point();
            let variant = match develop.as_ref() {
                Some(d) if d.before => StageVariant::Before,
                Some(d) if d.crop.active => StageVariant::CropTool,
                _ => StageVariant::Edited,
            };
            // The crop tool maps 1:1 onto the fitted full frame, so it never zooms.
            let view = zoom
                .as_ref()
                .filter(|_| variant != StageVariant::CropTool)
                .map_or_else(ViewState::default, |zoom| zoom.view());
            let interacting = zoom
                .as_ref()
                .is_some_and(|zoom| zoom.interacting(ui.input(|i| i.time)));
            let geo = geometry(snapshot, rect, ppp, view, variant);
            // The live border preview: the photo is rendered smaller, inside the margin colour.
            let params = snapshot.editor.document.params.as_ref();
            let border_preview = develop
                .as_ref()
                .filter(|_| variant == StageVariant::Edited && view.is_fit())
                .and(params.zip(output_dims(snapshot, variant)))
                .and_then(|(p, dims)| border::preview(rect, dims, &p.border));
            let frame_rect = border_preview.as_ref().map_or(rect, |b| b.inner);
            photo.plan = if photo.message.is_some() {
                None
            } else {
                photo_plan(
                    snapshot,
                    frame_rect,
                    ppp,
                    photo.device_epoch,
                    view,
                    interacting,
                    variant,
                )
            };
            if let (Some(preview), Some(p), None) = (&border_preview, params, &photo.message) {
                ui.painter()
                    .rect_filled(preview.outer, 0, border::paint_color(&p.border));
            }
            let drawn = stage_content(
                ui,
                snapshot,
                photo,
                frame_rect,
                geo.as_ref(),
                variant,
                interacting,
            );
            if let Some(dims) = output_dims(snapshot, variant) {
                // The crop tool never zooms, so it gets no zoom readout.
                let zoomer = match (zoom.as_deref_mut(), geo.as_ref(), drawn.as_ref()) {
                    (Some(zoom), Some(geo), Some(_)) if variant != StageVariant::CropTool => {
                        Some(Zoomer {
                            zoom,
                            frame_px: geo.frame_px,
                            panel_px: geo.panel_px,
                        })
                    }
                    _ => None,
                };
                zoom_footer::show(ui, rect, dims, zoomer);
            }
            if variant == StageVariant::Before {
                paint_before_badge(ui, rect);
            }
            if let (Some(zoom), Some(drawn)) = (zoom.as_mut(), drawn.as_ref()) {
                zoom.note_drawn(drawn.rect);
            }
            if let (Some(d), Some(drawn), Some(params), StageVariant::CropTool) =
                (develop.as_mut(), drawn.as_ref(), params, variant)
            {
                crop_tool::overlay(ui, drawn.rect, params, d.crop, typing, d.commands);
            }
            if let (Some(zoom), Some(drawn), Some(geo), Some(stage)) =
                (zoom.as_mut(), drawn.as_ref(), geo.as_ref(), stage.as_ref())
            {
                if variant != StageVariant::CropTool {
                    zoom.handle_input(
                        ui,
                        stage,
                        drawn.rect,
                        geo.frame_px,
                        geo.panel_px,
                        ppp,
                        typing,
                    );
                    if !view.is_fit() && stage.hovered() {
                        ui.ctx().set_cursor_icon(if stage.dragged() {
                            CursorIcon::Grabbing
                        } else {
                            CursorIcon::Grab
                        });
                    }
                }
            }
            // Claim the panel area for this frame's layout regardless of what was drawn in it.
            ui.allocate_rect(rect, egui::Sense::hover());
        });
}

#[cfg(test)]
mod tests {
    use super::{fitted_draw, visible_rect};
    use core_model::viewport::{derive_view, ViewState};
    use egui::{pos2, Pos2, Rect};

    /// The fixture's settled frame for a 3:2 photo in the 820×625-px panel.
    const SETTLED_PX: [u32; 2] = [820, 547];
    const FRAME_PX: (u32, u32) = (6960, 4640);

    #[test]
    fn the_visible_box_is_what_the_window_frame_is_a_picture_of() {
        // A very wide stage with a slightly zoomed photo: the image overflows the panel in height
        // only, so the drawn box is the image's width and the panel's height — and that box is
        // exactly the render target (`out_px`), i.e. the frame lands 1:1.
        let panel = Rect::from_min_max(Pos2::ZERO, pos2(1800.0, 300.0));
        let view = ViewState {
            zoom: 1.5,
            center: [0.5, 0.5],
        };
        let derived = derive_view(view, FRAME_PX, (1800, 300));
        let drawn = visible_rect(panel, &derived, 1.0);
        assert_eq!(
            (drawn.width().round() as u32, drawn.height().round() as u32),
            derived.out_px
        );
        assert_eq!(
            derived.out_px,
            (675, 300),
            "the image is panel-limited in height"
        );
        assert!(
            drawn.min.x >= panel.min.x && drawn.max.x <= panel.max.x + 0.5,
            "{drawn:?} must stay inside {panel:?}"
        );
        assert!(
            (drawn.center().x - panel.center().x).abs() < 0.5,
            "the image is centred horizontally: {drawn:?}"
        );
    }

    #[test]
    fn a_half_size_interactive_frame_fills_the_settled_rect() {
        // The DARKROOM-163 tier: during a drag the worker renders half size. Drawing the texture
        // at its own size would shrink the picture to a quarter of its area and halve the footer
        // zoom; the fit is taken from the panel and the frame's aspect instead.
        let rect = Rect::from_min_max(egui::Pos2::ZERO, pos2(820.0, 625.0));
        let (settled, settled_w) = fitted_draw(rect, 1.0, SETTLED_PX);
        let (drag, drag_w) = fitted_draw(rect, 1.0, [410, 273]);
        assert_eq!(settled_w, drag_w);
        assert_eq!(settled.center(), drag.center());
        assert_eq!(settled_w, 820);
        assert!((settled.width() - 820.0).abs() <= 0.5);
        // The half-size frame's rounded aspect (410/273) differs from 820/547 by <0.1%: at most
        // one pixel of height, invisible at the stage's scale.
        assert!((settled.height() - drag.height()).abs() <= 1.0);
    }

    #[test]
    fn the_draw_rect_is_the_fit_view_in_points_at_any_pixel_density() {
        // 410×312.5 points at ppp 2 is the same 820×625-px panel: the same frame lands at
        // 410×273.5 points, i.e. the physical fit divided by the density.
        let rect = Rect::from_min_max(egui::Pos2::ZERO, pos2(410.0, 312.5));
        let (draw, width) = fitted_draw(rect, 2.0, SETTLED_PX);
        assert_eq!(width, 820);
        assert!((draw.width() - 410.0).abs() < 0.01);
        assert!((draw.height() - 273.5).abs() < 0.01);
    }

    #[test]
    fn a_degenerate_texture_still_lands_on_a_real_rect() {
        let rect = Rect::from_min_max(egui::Pos2::ZERO, pos2(820.0, 625.0));
        let (draw, width) = fitted_draw(rect, 1.0, [0, 0]);
        assert_eq!(width, 625);
        assert!((draw.height() - 625.0).abs() < 0.01);
    }
}
