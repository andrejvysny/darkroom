//! Zoom/pan input for the Develop stage: the stage's [`ViewState`] plus the pointer and
//! keyboard gestures that change it (pinch / Ctrl-Cmd-scroll, plain-scroll and drag pan,
//! double-click and bare `Z` for 1:1). All the geometry is `core_model::viewport`; this only
//! decides *when* the view changes.

use core_app::state::ApplicationSnapshot;
use core_model::viewport::{one_to_one_zoom, pan_by, zoom_at, ViewState};
use std::time::Duration;

/// How long after the last view change the stage still asks for the cheap interactive tier.
const SETTLE: f64 = 0.2;
/// Zoom multiplier per mouse-wheel notch.
const WHEEL_STEP: f64 = 1.25;
/// Slightly longer than [`SETTLE`]: guarantees one more frame after the gesture settles, which
/// is the frame that asks for the full-size, histogram-bearing render.
const REPAINT: Duration = Duration::from_millis(220);

/// Transient zoom state of the stage, owned by [`super::DevelopUi`]. A different photo (or a new
/// editor session) always starts at fit: a stale window into the previous image must never be
/// applied to the next one.
#[derive(Default)]
pub(crate) struct ZoomUi {
    view: ViewState,
    subject: Option<(i64 /* image id */, u64 /* editor session */)>,
    interacting_until: f64,
    /// Where the frame was drawn last time input was handled (test/automation hook: it says
    /// where on screen the image is, i.e. where a gesture has to land).
    image_rect: Option<egui::Rect>,
}

impl ZoomUi {
    pub(crate) fn view(&self) -> ViewState {
        self.view
    }

    /// The drawn image rect of the last frame, in points.
    pub(crate) fn image_rect(&self) -> Option<egui::Rect> {
        self.image_rect
    }

    /// Remember where the frame was drawn, so gestures and automation know where the image is.
    pub(crate) fn note_drawn(&mut self, image_rect: egui::Rect) {
        self.image_rect = Some(image_rect);
    }

    /// Back to fit (the loupe calls this whenever it is not shown).
    pub(crate) fn reset(&mut self) {
        self.view = ViewState::default();
        self.interacting_until = 0.0;
    }

    /// Drop the view when the stage's subject changed; keep it otherwise (this runs every frame).
    pub(crate) fn sync(&mut self, snapshot: &ApplicationSnapshot) {
        let subject = snapshot
            .editor
            .selected
            .map(|id| (id.0, snapshot.editor.editor_session.0));
        if subject != self.subject {
            self.subject = subject;
            self.view = ViewState::default();
            self.interacting_until = 0.0;
        }
    }

    /// While a gesture is in flight the stage asks for half-size interactive frames; the trailing
    /// repaint makes sure the settled frame is asked for once this goes false again.
    pub(crate) fn interacting(&self, now: f64) -> bool {
        now < self.interacting_until
    }

    /// One frame of stage input. `image_rect` is where the presented frame is drawn, in points —
    /// i.e. the TOP-LEFT of the visible image area, which is what the viewport math anchors on.
    /// `typing` is the root's "a text edit owns the keyboard" flag: bare `Z` must not steal a
    /// letter the user is typing.
    // The gesture inputs are one bundle; splitting them across calls would only thread the same
    // five geometry arguments around.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_input(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        image_rect: egui::Rect,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
        ppp: f32,
        typing: bool,
    ) {
        if self.pinch_zoom(ui, response, image_rect, frame_px, panel_px, ppp) {
            return;
        }
        if self.wheel_zoom(ui, response, image_rect, frame_px, panel_px, ppp) {
            return;
        }
        self.scroll_pan(ui, response, frame_px, panel_px, ppp);
        self.drag_pan(ui, response, frame_px, panel_px, ppp);
        // Double-click and bare `Z` toggle fit ↔ 1:1 (anchored at the pointer / image centre).
        if response.double_clicked() {
            if let Some(anchor) = anchor_px(ui, response, image_rect, ppp) {
                self.toggle(ui, anchor, frame_px, panel_px);
            }
            return;
        }
        if !typing && bare_z(ui) {
            let centre = [
                f64::from(image_rect.width()) * f64::from(ppp) / 2.0,
                f64::from(image_rect.height()) * f64::from(ppp) / 2.0,
            ];
            self.toggle(ui, centre, frame_px, panel_px);
        }
    }

    /// Pinch or Ctrl/Cmd+scroll, anchored at the pointer. True when it consumed this frame, so a
    /// trackpad gesture synthesised as a scroll as well does not pan at the same time.
    fn pinch_zoom(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        image_rect: egui::Rect,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
        ppp: f32,
    ) -> bool {
        if !response.hovered() {
            return false;
        }
        let delta = ui.input(|i| i.zoom_delta());
        if delta == 1.0 {
            return false;
        }
        let Some(anchor) = anchor_px(ui, response, image_rect, ppp) else {
            return true;
        };
        let next = zoom_at(
            self.view,
            self.view.zoom * f64::from(delta),
            anchor,
            frame_px,
            panel_px,
        );
        self.commit(ui, next);
        true
    }

    /// A physical mouse wheel zooms at the pointer, `WHEEL_STEP` per notch. True when a wheel
    /// event was seen, so the same frame does not also pan.
    fn wheel_zoom(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        image_rect: egui::Rect,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
        ppp: f32,
    ) -> bool {
        if !response.hovered() {
            return false;
        }
        let Some(notches) = wheel_notches(ui) else {
            return false;
        };
        if let Some(anchor) = anchor_px(ui, response, image_rect, ppp) {
            let factor = WHEEL_STEP.powf(f64::from(notches));
            let next = zoom_at(
                self.view,
                self.view.zoom * factor,
                anchor,
                frame_px,
                panel_px,
            );
            self.commit(ui, next);
        }
        true
    }

    /// Plain two-finger scroll pans the image the same way a drag of that delta would.
    fn scroll_pan(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
        ppp: f32,
    ) {
        if self.view.is_fit() || !response.hovered() {
            return;
        }
        let delta = trackpad_delta(ui);
        if delta == egui::Vec2::ZERO {
            return;
        }
        self.commit(ui, pan_by(self.view, to_px(delta, ppp), frame_px, panel_px));
    }

    /// Drag-pan; at fit there is nothing to pan (and the drag is left to whatever widget is under
    /// the pointer).
    fn drag_pan(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
        ppp: f32,
    ) {
        if self.view.is_fit() || !response.dragged() {
            return;
        }
        self.commit(
            ui,
            pan_by(
                self.view,
                to_px(response.drag_delta(), ppp),
                frame_px,
                panel_px,
            ),
        );
    }

    fn toggle(
        &mut self,
        ui: &egui::Ui,
        anchor: [f64; 2],
        frame_px: (u32, u32),
        panel_px: (u32, u32),
    ) {
        let next = if self.view.is_fit() {
            zoom_at(
                self.view,
                one_to_one_zoom(frame_px, panel_px),
                anchor,
                frame_px,
                panel_px,
            )
        } else {
            ViewState::default()
        };
        self.commit(ui, next);
    }

    /// Record a view change and schedule the frames it needs: the interactive ones while the
    /// gesture lasts, then the settled one. An unchanged view (a pan against a clamped edge)
    /// costs nothing.
    fn commit(&mut self, ui: &egui::Ui, next: ViewState) {
        if next == self.view {
            return;
        }
        self.view = next;
        self.interacting_until = ui.input(|i| i.time) + SETTLE;
        ui.ctx().request_repaint_after(REPAINT);
    }
}

/// Net wheel notches this frame (up = positive) from physical-wheel events without Cmd/Ctrl, or
/// `None` when there were none. Read from raw events: `smooth_scroll_delta` spreads a wheel
/// notch over later frames and would pan them.
fn wheel_notches(ui: &egui::Ui) -> Option<f32> {
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
fn trackpad_delta(ui: &egui::Ui) -> egui::Vec2 {
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
fn bare_z(ui: &egui::Ui) -> bool {
    ui.ctx()
        .input_mut(|i| i.modifiers.is_none() && i.consume_key(egui::Modifiers::NONE, egui::Key::Z))
}

/// The pointer in physical px relative to the top-left of the visible image area.
fn anchor_px(
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
fn to_px(delta: egui::Vec2, ppp: f32) -> [f64; 2] {
    [
        f64::from(delta.x) * f64::from(ppp),
        f64::from(delta.y) * f64::from(ppp),
    ]
}

#[cfg(test)]
mod tests {
    use super::ZoomUi;
    use core_app::state::{
        ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
        LibrarySnapshot, SettingsSnapshot,
    };
    use core_model::catalog::ImageRow;
    use core_model::identity::{EditRevision, ImageId, ProcessVersion};
    use core_model::viewport::ViewState;
    use std::sync::Arc;

    fn snapshot(image: Option<i64>, session: u64) -> ApplicationSnapshot {
        ApplicationSnapshot {
            revision: 1,
            running: true,
            catalog_ready: true,
            catalog_epoch: Default::default(),
            library: LibrarySnapshot::default(),
            jobs: JobsSnapshot::default(),
            settings: SettingsSnapshot::default(),
            error: None,
            editor: EditorSnapshot {
                selected: image.map(ImageId),
                image: image.map(|id| ImageRow {
                    id,
                    width: Some(6960),
                    height: Some(4640),
                    ..Default::default()
                }),
                editor_session: core_model::identity::EditorSession(session),
                document: DocumentView {
                    status: DocumentStatus::Ready,
                    params: Some(Arc::new(core_model::DevelopParams::default())),
                    revision: Some(EditRevision(1)),
                    process_version: Some(ProcessVersion(6)),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    const ZOOMED: ViewState = ViewState {
        zoom: 3.0,
        center: [0.25, 0.75],
    };

    /// A stage whose subject is `image`/`session` and whose view is [`ZOOMED`], i.e. the state
    /// after the user zoomed into that photo.
    fn zoomed(image: i64, session: u64) -> ZoomUi {
        ZoomUi {
            view: ZOOMED,
            subject: Some((image, session)),
            interacting_until: 0.0,
            image_rect: None,
        }
    }

    #[test]
    fn a_new_photo_resets_the_view_to_fit() {
        let mut zoom = zoomed(1, 1);
        zoom.sync(&snapshot(Some(2), 1));
        assert_eq!(zoom.view, ViewState::default());
    }

    #[test]
    fn the_same_photo_keeps_the_view() {
        let mut zoom = zoomed(1, 1);
        zoom.sync(&snapshot(Some(1), 1));
        assert_eq!(zoom.view, ZOOMED);
    }

    #[test]
    fn a_new_editor_session_resets_the_view_too() {
        // Same image id under a reopened editor is a fresh subject: the old window is meaningless.
        let mut zoom = zoomed(1, 1);
        zoom.sync(&snapshot(Some(1), 2));
        assert_eq!(zoom.view, ViewState::default());
    }

    #[test]
    fn a_cleared_selection_resets_the_view_too() {
        let mut zoom = zoomed(1, 1);
        zoom.sync(&snapshot(None, 1));
        assert_eq!(zoom.view, ViewState::default());
    }

    #[test]
    fn interacting_expires_after_the_settle_window() {
        let zoom = ZoomUi {
            interacting_until: 5.0,
            ..Default::default()
        };
        assert!(zoom.interacting(4.9));
        assert!(!zoom.interacting(5.0));
    }
}
