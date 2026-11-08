//! The stage footer: a clickable zoom readout ("Fit" / "NN%" of actual pixels) that opens a
//! preset list, followed by the output size and colour space.

use super::zoom::ZoomUi;
use crate::theme;
use core_model::viewport::{max_zoom, one_to_one_zoom, ViewState, MIN_ZOOM};
use egui::{Align, Layout, Rect, RichText};

/// One footer menu entry: `zoom` is relative to fit, `None` = Fit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Preset {
    pub label: &'static str,
    pub zoom: Option<f64>,
}

/// Actual-pixel presets offered besides Fit.
const ACTUAL: [(&str, f64); 3] = [("100%", 1.0), ("200%", 2.0), ("400%", 4.0)];
/// Slack for comparing a preset's zoom against fit and the ceiling (400 % IS the ceiling).
const EPS: f64 = 1e-6;

/// Fit, plus every actual-pixel preset that is a real zoom-in (above fit) and within the ceiling.
/// A tiny image that already fits above 100 % therefore offers Fit alone.
pub(crate) fn presets(frame_px: (u32, u32), panel_px: (u32, u32)) -> Vec<Preset> {
    let one = one_to_one_zoom(frame_px, panel_px);
    let ceiling = max_zoom(frame_px, panel_px);
    let mut list = vec![Preset {
        label: "Fit",
        zoom: None,
    }];
    list.extend(ACTUAL.iter().filter_map(|&(label, actual)| {
        let zoom = actual * one;
        (zoom > MIN_ZOOM + EPS && zoom <= ceiling + EPS).then_some(Preset {
            label,
            zoom: Some(zoom.min(ceiling)),
        })
    }));
    list
}

/// "Fit" at fit, otherwise the zoom in percent of actual pixels.
pub(crate) fn readout(view: ViewState, frame_px: (u32, u32), panel_px: (u32, u32)) -> String {
    if view.is_fit() {
        return "Fit".to_owned();
    }
    let pct = view.zoom / one_to_one_zoom(frame_px, panel_px) * 100.0;
    format!("{}%", pct.round())
}

/// `"•  6960 × 4640  •  sRGB"`.
pub(crate) fn size_text(dims: (u32, u32)) -> String {
    format!("•  {} × {}  •  sRGB", dims.0, dims.1)
}

/// The zoom a footer click may change: the stage's state plus the geometry it is measured in.
pub(crate) struct Zoomer<'a> {
    pub zoom: &'a mut ZoomUi,
    pub frame_px: (u32, u32),
    pub panel_px: (u32, u32),
}

/// Paint the footer at the bottom-left of `stage`. Without a `zoomer` (nothing zoomable drawn)
/// only the size shows.
pub(crate) fn show(ui: &mut egui::Ui, stage: Rect, dims: (u32, u32), zoomer: Option<Zoomer<'_>>) {
    let bar = Rect::from_min_max(
        stage.left_bottom() + egui::vec2(14.0, -30.0),
        stage.right_bottom() + egui::vec2(-14.0, -6.0),
    );
    let builder = egui::UiBuilder::new()
        .max_rect(bar)
        .layout(Layout::left_to_right(Align::Center));
    ui.scope_builder(builder, |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let Some(zoomer) = zoomer {
            readout_menu(ui, zoomer);
        }
        ui.label(
            RichText::new(size_text(dims))
                .monospace()
                .size(11.0)
                .color(theme::T3),
        );
    });
}

/// The readout button and its preset popup.
fn readout_menu(ui: &mut egui::Ui, zoomer: Zoomer<'_>) {
    let Zoomer {
        zoom,
        frame_px,
        panel_px,
    } = zoomer;
    let text = readout(zoom.view(), frame_px, panel_px);
    let button = ui
        .add(
            egui::Button::new(RichText::new(text).monospace().size(11.0).color(theme::T2))
                .frame(false),
        )
        .on_hover_text("Zoom");
    let picked = egui::Popup::menu(&button)
        .align(egui::RectAlign::TOP_START)
        .show(|ui| {
            presets(frame_px, panel_px).into_iter().find(|preset| {
                let clicked = ui.button(preset.label).clicked();
                if clicked {
                    // Explicit: a keyboard/AccessKit activation is not a pointer click, which is
                    // all the popup's own close-on-click watches.
                    ui.close();
                }
                clicked
            })
        })
        .and_then(|menu| menu.inner);
    if let Some(preset) = picked {
        zoom.jump(ui, preset.zoom, frame_px, panel_px);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: (u32, u32) = (6960, 4640);
    const PANEL: (u32, u32) = (1392, 928);

    fn labels(frame: (u32, u32), panel: (u32, u32)) -> Vec<&'static str> {
        presets(frame, panel).iter().map(|p| p.label).collect()
    }

    #[test]
    fn a_big_photo_offers_every_preset() {
        // one-to-one = 5, so 100/200/400 % are zoom 5/10/20 and 400 % is exactly the ceiling.
        assert_eq!(labels(FRAME, PANEL), ["Fit", "100%", "200%", "400%"]);
        let list = presets(FRAME, PANEL);
        assert_eq!(list[1].zoom, Some(one_to_one_zoom(FRAME, PANEL)));
        assert_eq!(list[3].zoom, Some(max_zoom(FRAME, PANEL)));
    }

    #[test]
    fn presets_below_fit_or_above_the_ceiling_are_left_out() {
        // one-to-one = 0.5: 100 % would be BELOW fit and 200 % is fit itself; only 400 % zooms.
        assert_eq!(labels((600, 400), (1200, 800)), ["Fit", "400%"]);
        // Already above the ceiling at fit (one-to-one = 0.1): nothing to zoom into.
        assert_eq!(labels((200, 100), (1200, 800)), ["Fit"]);
        assert_eq!(labels((1, 1), (1200, 800)), ["Fit"]);
    }

    #[test]
    fn the_readout_is_fit_or_percent_of_actual_pixels() {
        let one = one_to_one_zoom(FRAME, PANEL);
        assert_eq!(readout(ViewState::default(), FRAME, PANEL), "Fit");
        for (zoom, text) in [(one, "100%"), (2.0 * one, "200%"), (4.0 * one, "400%")] {
            let view = ViewState {
                zoom,
                center: [0.5, 0.5],
            };
            assert_eq!(readout(view, FRAME, PANEL), text);
        }
        let view = ViewState {
            zoom: 2.0,
            center: [0.5, 0.5],
        };
        assert_eq!(readout(view, FRAME, PANEL), "40%");
    }

    #[test]
    fn the_size_text_names_dims_and_space() {
        assert_eq!(size_text((6960, 4640)), "•  6960 × 4640  •  sRGB");
    }
}
