//! Raw-input readers for the stage's zoom/pan gestures (what the events *are*; what they do
//! is `zoom.rs`).

/// Net wheel notches this frame (up = positive) from physical-wheel events without Cmd/Ctrl, or
/// `None` when there were none. Read from raw events: `smooth_scroll_delta` spreads a wheel
/// notch over later frames and would pan them.
pub(super) fn wheel_notches(ui: &egui::Ui) -> Option<f32> {
    ui.input(|i| {
        let mut seen = false;
        let mut sum = 0.0;
        for event in &i.raw.events {
            if let egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page,
                delta,
                source: egui::MouseWheelSource::Wheel,
                modifiers,
                ..
            } = event
            {
                if !modifiers.command {
                    seen = true;
                    sum += delta.y;
                }
            }
        }
        seen.then_some(sum)
    })
}

/// Scroll points this frame that did not come from a physical wheel (trackpad two-finger pan).
pub(super) fn trackpad_delta(ui: &egui::Ui) -> egui::Vec2 {
    ui.input(|i| {
        i.raw
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta,
                    source,
                    ..
                } if *source != egui::MouseWheelSource::Wheel => Some(*delta),
                _ => None,
            })
            .fold(egui::Vec2::ZERO, |acc, d| acc + d)
    })
}

/// Bare `Z` pressed: no modifiers, and consumed so nothing else sees it. `consume_key` alone is
/// not enough — it would also fire with Cmd held, and Cmd+Z is undo (`super::shortcuts`).
pub(super) fn bare_z(ui: &egui::Ui) -> bool {
    ui.ctx()
        .input_mut(|i| i.modifiers.is_none() && i.consume_key(egui::Modifiers::NONE, egui::Key::Z))
}

/// The pointer in physical px relative to the top-left of the visible image area.
pub(super) fn anchor_px(
    ui: &egui::Ui,
    response: &egui::Response,
    image_rect: egui::Rect,
    ppp: f32,
) -> Option<[f64; 2]> {
    let pos = response
        .hover_pos()
        .or_else(|| ui.ctx().pointer_hover_pos())?;
    Some([
        f64::from(pos.x - image_rect.min.x) * f64::from(ppp),
        f64::from(pos.y - image_rect.min.y) * f64::from(ppp),
    ])
}

/// A points delta (drag or scroll) in physical px.
pub(super) fn to_px(delta: egui::Vec2, ppp: f32) -> [f64; 2] {
    [
        f64::from(delta.x) * f64::from(ppp),
        f64::from(delta.y) * f64::from(ppp),
    ]
}
