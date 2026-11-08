//! Zoom/pan input for the Develop stage: the stage's [`ViewState`] plus the pointer and
//! keyboard gestures that change it (pinch / Ctrl-Cmd-scroll, plain-scroll and drag pan,
//! double-click and bare `Z` for 1:1). All the geometry is `core_model::viewport`; this only
//! decides *when* the view changes.

use core_app::state::ApplicationSnapshot;
use core_model::viewport::{
    derive_view, max_zoom, one_to_one_zoom, pan_by, zoom_at, ViewState, MIN_ZOOM,
};
use std::time::Duration;

use super::zoom_input::{anchor_px, bare_z, to_px, trackpad_delta, wheel_notches};

/// How long after the last view change the stage still asks for the cheap interactive tier.
const SETTLE: f64 = 0.2;
/// Zoom multiplier per mouse-wheel notch.
const WHEEL_STEP: f64 = 1.25;
/// Time constant of the wheel-zoom ease (log-space exponential): ~100 ms to settle, short enough
/// to feel direct, long enough that a notch is not a jump.
const EASE_TAU: f64 = 0.035;
/// Below this remaining log-ratio the ease snaps onto its target (and ends).
const SNAP_LOG: f64 = 1e-3;
/// Longest frame step the ease will integrate, so a stalled frame does not teleport the zoom.
const MAX_DT: f64 = 0.1;
/// Slightly longer than [`SETTLE`]: guarantees one more frame after the gesture settles, which
/// is the frame that asks for the full-size, histogram-bearing render.
const REPAINT: Duration = Duration::from_millis(220);

/// Where a wheel zoom is heading: the zoom and the screen point (in points) that must stay put.
/// The point is kept in screen space, not relative to the image, because the image rect itself
/// moves while the zoom eases.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WheelTarget {
    zoom: f64,
    pointer: egui::Pos2,
}

/// Transient zoom state of the stage, owned by [`super::DevelopUi`]. A different photo (or a new
/// editor session) always starts at fit: a stale window into the previous image must never be
/// applied to the next one.
#[derive(Default)]
pub(crate) struct ZoomUi {
    view: ViewState,
    subject: Option<(i64 /* image id */, u64 /* editor session */)>,
    interacting_until: f64,
    /// An eased wheel zoom in flight.
    target: Option<WheelTarget>,
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
        self.target = None;
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
            self.target = None;
        }
    }

    /// While a gesture is in flight the stage asks for half-size interactive frames; the trailing
    /// repaint makes sure the settled frame is asked for once this goes false again.
    pub(crate) fn interacting(&self, now: f64) -> bool {
        now < self.interacting_until || self.target.is_some()
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
            // Pinch already tracks the fingers; an eased wheel zoom would fight it.
            self.target = None;
            return;
        }
        let wheel = self.wheel_zoom(ui, response, frame_px, panel_px);
        self.ease(ui, image_rect, frame_px, panel_px, ppp);
        if wheel {
            return;
        }
        self.scroll_pan(ui, response, frame_px, panel_px, ppp);
        self.drag_pan(ui, response, frame_px, panel_px, ppp);
        // Double-click and bare `Z` toggle fit ↔ 1:1 (anchored at the pointer / image centre).
        if response.double_clicked() {
            self.target = None;
            if let Some(anchor) = anchor_px(ui, response, image_rect, ppp) {
                self.toggle(ui, anchor, frame_px, panel_px);
            }
            return;
        }
        if !typing && bare_z(ui) {
            self.target = None;
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

    /// A physical mouse wheel sets (or moves) the zoom TARGET, `WHEEL_STEP` per notch, anchored
    /// at the pointer; [`Self::ease`] walks the view there. Notches stack on the target, not on
    /// the half-way view, so a fast spin lands exactly where the notches say. True when a wheel
    /// event was seen, so the same frame does not also pan.
    fn wheel_zoom(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
    ) -> bool {
        if !response.hovered() {
            return false;
        }
        let Some(notches) = wheel_notches(ui) else {
            return false;
        };
        let Some(pointer) = response
            .hover_pos()
            .or_else(|| ui.ctx().pointer_hover_pos())
        else {
            return true;
        };
        let base = self.target.map_or(self.view.zoom, |target| target.zoom);
        let zoom = (base * WHEEL_STEP.powf(f64::from(notches)))
            .clamp(MIN_ZOOM, max_zoom(frame_px, panel_px));
        // A target equal to where the view already is (wheel-out at fit, wheel-in at the
        // ceiling) is no gesture at all.
        self.target = ((zoom / self.view.zoom).ln().abs() >= SNAP_LOG)
            .then_some(WheelTarget { zoom, pointer });
        true
    }

    /// One step of the eased wheel zoom: the view moves toward the target in log space (so zooming
    /// in and out feel alike) with the target's anchor held fixed. Keeps the gesture marked as
    /// interacting and the frames coming until it lands.
    fn ease(
        &mut self,
        ui: &egui::Ui,
        image_rect: egui::Rect,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
        ppp: f32,
    ) {
        let Some(target) = self.target else {
            return;
        };
        let dt = f64::from(ui.input(|i| i.unstable_dt)).clamp(1e-3, MAX_DT);
        let remaining = (target.zoom / self.view.zoom).ln();
        let mut zoom = self.view.zoom * (remaining * (1.0 - (-dt / EASE_TAU).exp())).exp();
        let landed = (target.zoom / zoom).ln().abs() < SNAP_LOG;
        if landed {
            zoom = target.zoom;
        }
        let anchor = [
            f64::from(target.pointer.x - image_rect.min.x) * f64::from(ppp),
            f64::from(target.pointer.y - image_rect.min.y) * f64::from(ppp),
        ];
        let next = zoom_at(self.view, zoom, anchor, frame_px, panel_px);
        // No progress and not landed: the clamp (a resize, say) moved the goalposts; give up
        // rather than animate forever.
        if landed || next == self.view {
            self.target = None;
        } else {
            ui.ctx().request_repaint();
        }
        self.commit(ui, next);
    }

    /// Jump to a footer preset, anchored at the centre of the visible image area. `None` is Fit;
    /// `Some(zoom)` is a zoom relative to fit.
    pub(crate) fn jump(
        &mut self,
        ui: &egui::Ui,
        zoom: Option<f64>,
        frame_px: (u32, u32),
        panel_px: (u32, u32),
    ) {
        self.target = None;
        let next = match zoom {
            None => ViewState::default(),
            Some(zoom) => {
                let out = derive_view(self.view, frame_px, panel_px).out_px;
                let centre = [f64::from(out.0) / 2.0, f64::from(out.1) / 2.0];
                zoom_at(self.view, zoom, centre, frame_px, panel_px)
            }
        };
        self.commit(ui, next);
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
            target: None,
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
