//! The interactive crop tool: the draggable crop rectangle drawn over the full frame, its drag
//! gesture (Begin/Update/Commit, Esc cancels) and the aspect presets. The drag math is the legacy
//! `CropOverlay.tsx` verbatim: edges are tracked as l/r/t/b in the rotated frame, measured from
//! the crop at drag start, and clamped to the frame with a minimum half-extent.

use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, EditIntent};
use core_model::{Crop, DevelopParams};
use egui::{Color32, CursorIcon, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind, Ui};
use std::sync::Arc;

/// Smallest crop half-extent (4% of the edge).
const MIN_HALF: f32 = 0.02;
const HANDLE: f32 = 14.0;

/// Aspect presets in the legacy panel order; `0.0` = the full frame.
pub(super) const ASPECTS: [(&str, f32); 8] = [
    ("Full", 0.0),
    ("1:1", 1.0),
    ("4:5", 4.0 / 5.0),
    ("5:4", 5.0 / 4.0),
    ("3:2", 3.0 / 2.0),
    ("2:3", 2.0 / 3.0),
    ("4:3", 4.0 / 3.0),
    ("16:9", 16.0 / 9.0),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Handle {
    Move,
    Nw,
    Ne,
    Sw,
    Se,
}

/// Crop edges in the normalized rotated frame.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Edges {
    l: f32,
    r: f32,
    t: f32,
    b: f32,
}

impl Edges {
    fn of(crop: &Crop) -> Self {
        Self {
            l: crop.cx - crop.hw,
            r: crop.cx + crop.hw,
            t: crop.cy - crop.hh,
            b: crop.cy + crop.hh,
        }
    }

    fn crop(self, base: &Crop) -> Crop {
        Crop {
            cx: (self.l + self.r) / 2.0,
            cy: (self.t + self.b) / 2.0,
            hw: (self.r - self.l) / 2.0,
            hh: (self.b - self.t) / 2.0,
            ..*base
        }
    }

    /// The edges after dragging `handle` by the normalized pointer delta (`dx`, `dy`).
    fn dragged(self, handle: Handle, dx: f32, dy: f32) -> Self {
        let Self {
            mut l,
            mut r,
            mut t,
            mut b,
        } = self;
        if handle == Handle::Move {
            let (w, h) = (self.r - self.l, self.b - self.t);
            l = (self.l + dx).max(0.0).min(1.0 - w);
            t = (self.t + dy).max(0.0).min(1.0 - h);
            r = l + w;
            b = t + h;
        } else {
            if matches!(handle, Handle::Nw | Handle::Sw) {
                l = (self.l + dx).max(0.0).min(self.r - 2.0 * MIN_HALF);
            }
            if matches!(handle, Handle::Ne | Handle::Se) {
                r = (self.r + dx).min(1.0).max(self.l + 2.0 * MIN_HALF);
            }
            if matches!(handle, Handle::Nw | Handle::Ne) {
                t = (self.t + dy).max(0.0).min(self.b - 2.0 * MIN_HALF);
            }
            if matches!(handle, Handle::Sw | Handle::Se) {
                b = (self.b + dy).min(1.0).max(self.t + 2.0 * MIN_HALF);
            }
        }
        Self { l, r, t, b }
    }

    fn screen(self, frame: Rect) -> Rect {
        let at = |u: f32, v: f32| {
            Pos2::new(
                frame.left() + u * frame.width(),
                frame.top() + v * frame.height(),
            )
        };
        Rect::from_min_max(at(self.l, self.t), at(self.r, self.b))
    }
}

/// A pointer drag in flight.
struct Drag {
    handle: Handle,
    /// Where the press landed and the crop edges at that moment.
    origin: Pos2,
    start: Edges,
    last_sent: Option<Edges>,
}

/// Crop-tool UI state, owned by [`super::DevelopUi`].
#[derive(Default)]
pub(crate) struct CropState {
    pub(super) active: bool,
    drag: Option<Drag>,
    /// Esc cancelled the drag; ignore the rest of the pointer drag.
    cancelled: bool,
    subject: Option<(i64, u64)>,
}

impl CropState {
    /// Leave crop mode when the photo changes: a crop box over the previous image must never
    /// be applied to the next one.
    pub(super) fn sync(&mut self, snapshot: &ApplicationSnapshot) {
        let subject = snapshot
            .editor
            .selected
            .map(|id| (id.0, snapshot.editor.editor_session.0));
        if subject != self.subject {
            self.subject = subject;
            self.active = false;
            self.drag = None;
            self.cancelled = false;
        }
    }

    /// Exit crop mode, cancelling a drag in flight.
    pub(super) fn leave(&mut self, commands: &mut Vec<Command>) {
        if self.drag.take().is_some() {
            commands.push(Command::Edit(EditIntent::CancelGesture));
        }
        self.active = false;
        self.cancelled = false;
    }
}

/// The centered crop of `target` aspect (W/H) inside a frame of `image_aspect`: legacy
/// `setAspect`, as `(hw, hh)`. `target <= 0` is the full frame.
pub(super) fn aspect_half_extents(target: f32, image_aspect: f32) -> (f32, f32) {
    if target <= 0.0 {
        return (0.5, 0.5);
    }
    let k = target / image_aspect;
    if k >= 1.0 {
        (0.5, 0.5 / k)
    } else {
        (0.5 * k, 0.5)
    }
}

/// Aspect (W/H) of the full rotated frame from the catalog dims, `None` when unknown.
pub(super) fn rotated_aspect(dims: Option<(u32, u32)>, rot90: i32) -> Option<f32> {
    let (w, h) = dims.filter(|&(w, h)| w > 0 && h > 0)?;
    let (w, h) = if rot90.rem_euclid(4) % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    };
    Some(w as f32 / h as f32)
}

/// The crop with the preset applied (centered, angle/rotation kept); `None` when the preset
/// cannot be computed yet (unknown dims for a ratio).
pub(super) fn preset_crop(base: &Crop, target: f32, dims: Option<(u32, u32)>) -> Option<Crop> {
    let (hw, hh) = if target <= 0.0 {
        (0.5, 0.5)
    } else {
        aspect_half_extents(target, rotated_aspect(dims, base.rot90)?)
    };
    Some(Crop {
        cx: 0.5,
        cy: 0.5,
        hw,
        hh,
        ..*base
    })
}

fn paint(ui: &Ui, frame: Rect, crop: Rect) {
    let painter = ui.painter();
    let dim = Color32::from_black_alpha(128);
    for outside in [
        Rect::from_min_max(frame.min, Pos2::new(frame.right(), crop.top())),
        Rect::from_min_max(Pos2::new(frame.left(), crop.bottom()), frame.max),
        Rect::from_min_max(
            Pos2::new(frame.left(), crop.top()),
            Pos2::new(crop.left(), crop.bottom()),
        ),
        Rect::from_min_max(
            Pos2::new(crop.right(), crop.top()),
            Pos2::new(frame.right(), crop.bottom()),
        ),
    ] {
        if outside.width() > 0.0 && outside.height() > 0.0 {
            painter.rect_filled(outside, 0, dim);
        }
    }
    let guide = Stroke::new(1.0, Color32::from_white_alpha(77));
    for f in [1.0 / 3.0, 2.0 / 3.0] {
        let x = crop.left() + crop.width() * f;
        let y = crop.top() + crop.height() * f;
        painter.line_segment(
            [Pos2::new(x, crop.top()), Pos2::new(x, crop.bottom())],
            guide,
        );
        painter.line_segment(
            [Pos2::new(crop.left(), y), Pos2::new(crop.right(), y)],
            guide,
        );
    }
    painter.rect_stroke(
        crop,
        0,
        Stroke::new(1.0, Color32::from_white_alpha(230)),
        StrokeKind::Outside,
    );
}

fn paint_handle(ui: &Ui, rect: Rect) {
    ui.painter()
        .rect_filled(rect, 2, Color32::from_black_alpha(90));
    ui.painter().rect_stroke(
        rect,
        2,
        Stroke::new(2.0, Color32::WHITE),
        StrokeKind::Inside,
    );
}

/// Draws and drives the tool over `frame` (the displayed full frame, in points).
pub(super) fn overlay(
    ui: &mut Ui,
    frame: Rect,
    params: &Arc<DevelopParams>,
    st: &mut CropState,
    typing: bool,
    commands: &mut Vec<Command>,
) {
    let crop = Edges::of(&params.crop).screen(frame);
    paint(ui, frame, crop);
    let corners = [
        (Handle::Nw, crop.left_top(), CursorIcon::ResizeNwSe),
        (Handle::Ne, crop.right_top(), CursorIcon::ResizeNeSw),
        (Handle::Sw, crop.left_bottom(), CursorIcon::ResizeNeSw),
        (Handle::Se, crop.right_bottom(), CursorIcon::ResizeNwSe),
    ];
    // Registered body first so the corner handles win the hit test where they overlap it.
    let mut zones = vec![(
        Handle::Move,
        ui.interact(crop, Id::unique(("crop_tool", 0)), Sense::drag())
            .on_hover_cursor(CursorIcon::Move),
    )];
    for (i, (handle, at, cursor)) in corners.into_iter().enumerate() {
        let rect = Rect::from_center_size(at, egui::vec2(HANDLE, HANDLE));
        let id = Id::unique(("crop_tool", i + 1));
        zones.push((
            handle,
            ui.interact(rect, id, Sense::drag()).on_hover_cursor(cursor),
        ));
        paint_handle(ui, rect);
    }
    let esc_cancelled =
        st.drag.is_some() && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
    if esc_cancelled {
        st.drag = None;
        st.cancelled = true;
        commands.push(Command::Edit(EditIntent::CancelGesture));
    }
    for (handle, response) in &zones {
        drive(ui, *handle, response, frame, params, st, commands);
    }
    if !typing && st.drag.is_none() && !esc_cancelled {
        let leave = ui.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Escape)
                || i.consume_key(Modifiers::NONE, Key::Enter)
        });
        if leave {
            st.active = false;
        }
    }
}

fn drive(
    ui: &Ui,
    handle: Handle,
    response: &egui::Response,
    frame: Rect,
    params: &Arc<DevelopParams>,
    st: &mut CropState,
    commands: &mut Vec<Command>,
) {
    if response.drag_started() && st.drag.is_none() {
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        if let Some(origin) = origin {
            st.cancelled = false;
            st.drag = Some(Drag {
                handle,
                origin,
                start: Edges::of(&params.crop),
                last_sent: None,
            });
            commands.push(Command::Edit(EditIntent::BeginGesture));
        }
    }
    if response.dragged() && !st.cancelled {
        if let (Some(drag), Some(pos)) = (
            st.drag.as_mut().filter(|drag| drag.handle == handle),
            response.interact_pointer_pos(),
        ) {
            let dx = (pos.x - drag.origin.x) / frame.width().max(1.0);
            let dy = (pos.y - drag.origin.y) / frame.height().max(1.0);
            let next = drag.start.dragged(handle, dx, dy);
            if drag.last_sent != Some(next) {
                drag.last_sent = Some(next);
                let update = DevelopParams {
                    crop: next.crop(&params.crop),
                    ..(**params).clone()
                };
                commands.push(Command::Edit(EditIntent::UpdateGesture(Box::new(update))));
            }
        }
    }
    if response.drag_stopped() {
        if st.drag.as_ref().is_some_and(|drag| drag.handle == handle) {
            st.drag = None;
            commands.push(Command::Edit(EditIntent::CommitGesture));
        }
        st.cancelled = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_follow_the_legacy_set_aspect() {
        // 1:1 on a 3:2 frame: k = 2/3 -> a centered square of the short edge.
        let (hw, hh) = aspect_half_extents(1.0, 1.5);
        assert!((hw - 1.0 / 3.0).abs() < 1e-6 && hh == 0.5);
        // 16:9 on the same frame is wider than it: full width, reduced height.
        let (hw, hh) = aspect_half_extents(16.0 / 9.0, 1.5);
        assert_eq!(hw, 0.5);
        assert!((hh - 0.5 / (16.0 / 9.0 / 1.5)).abs() < 1e-6);
        assert_eq!(aspect_half_extents(0.0, 1.5), (0.5, 0.5));
    }

    #[test]
    fn rotation_swaps_the_frame_aspect_and_unknown_dims_have_none() {
        assert_eq!(rotated_aspect(Some((6000, 4000)), 0), Some(1.5));
        assert_eq!(rotated_aspect(Some((6000, 4000)), 1), Some(4000.0 / 6000.0));
        assert_eq!(
            rotated_aspect(Some((6000, 4000)), -1),
            Some(4000.0 / 6000.0)
        );
        assert_eq!(rotated_aspect(None, 0), None);
        assert_eq!(rotated_aspect(Some((0, 4000)), 0), None);
    }

    #[test]
    fn corner_drags_clamp_to_the_frame_and_the_minimum_size() {
        let full = Edges {
            l: 0.0,
            r: 1.0,
            t: 0.0,
            b: 1.0,
        };
        // Past the opposite corner: stops MIN_HALF*2 short of it.
        let shrunk = full.dragged(Handle::Nw, 2.0, 2.0);
        assert!((shrunk.l - (1.0 - 2.0 * MIN_HALF)).abs() < 1e-6);
        assert!((shrunk.t - (1.0 - 2.0 * MIN_HALF)).abs() < 1e-6);
        assert_eq!((shrunk.r, shrunk.b), (1.0, 1.0));
        // Outward drags cannot leave the frame.
        assert_eq!(full.dragged(Handle::Se, 0.5, 0.5), full);
    }

    #[test]
    fn moving_keeps_the_size_and_stops_at_the_frame() {
        let crop = Edges {
            l: 0.25,
            r: 0.75,
            t: 0.25,
            b: 0.75,
        };
        let moved = crop.dragged(Handle::Move, 0.9, -0.9);
        assert_eq!(
            moved,
            Edges {
                l: 0.5,
                r: 1.0,
                t: 0.0,
                b: 0.5
            }
        );
    }
}
