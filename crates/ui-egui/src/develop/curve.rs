//! Tone-curve editor (legacy `ToneCurve.tsx`): square plot with quarter grid and a luminance
//! histogram silhouette, the active channel's curve drawn by sampling core-model's own LUT builder
//! (so the line is exactly what the renderer applies), draggable control points, channel tabs.

use super::slider::Env;
use super::DevelopUi;
use crate::{histogram, theme};
use core_app::workflow::EditIntent;
use core_model::curve::build_lut;
use core_model::{CurvePoint, DevelopParams, ToneCurve};
use egui::{Align2, Color32, CursorIcon, FontId, Id, Key, Pos2, Rect, Sense, Stroke, Ui};

const PAD: f32 = 10.0;
const MAX_POINTS: usize = 16;
/// Minimum x gap between neighbours so ordering stays stable during a drag.
const EPS: f32 = 0.001;
const HIT_RADIUS: f32 = 8.0;
const TABS: [&str; 4] = ["RGB", "R", "G", "B"];

/// A pointer press inside the plot: which point (if any) it landed on.
#[derive(Clone)]
pub(crate) struct Grab {
    index: Option<usize>,
    last: Option<(f32, f32)>,
    /// The point list the drag moves within (includes a point inserted by this very drag, so the
    /// gesture never depends on how soon the snapshot reflects its own updates).
    base: Vec<CurvePoint>,
}

fn channel_color(channel: usize) -> Color32 {
    match channel {
        1 => Color32::from_rgb(0xdd, 0x66, 0x66),
        2 => Color32::from_rgb(0x66, 0xcc, 0x66),
        3 => Color32::from_rgb(0x66, 0x99, 0xdd),
        _ => theme::T1,
    }
}

fn channel(tc: &ToneCurve, ch: usize) -> &Vec<CurvePoint> {
    match ch {
        1 => &tc.r,
        2 => &tc.g,
        3 => &tc.b,
        _ => &tc.rgb,
    }
}

fn channel_mut(tc: &mut ToneCurve, ch: usize) -> &mut Vec<CurvePoint> {
    match ch {
        1 => &mut tc.r,
        2 => &mut tc.g,
        3 => &mut tc.b,
        _ => &mut tc.rgb,
    }
}

/// A stored channel with fewer than two points is the editable straight identity.
fn points_of(tc: &ToneCurve, ch: usize) -> Vec<CurvePoint> {
    let stored = channel(tc, ch);
    if stored.len() >= 2 {
        stored.clone()
    } else {
        vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }]
    }
}

/// `pts` with `p` inserted between the fixed endpoints (x-sorted, kept `EPS` clear of its
/// neighbours); `None` at the point limit or when there is no room.
fn inserted(pts: &[CurvePoint], p: CurvePoint) -> Option<(Vec<CurvePoint>, usize)> {
    if pts.len() >= MAX_POINTS || pts.len() < 2 {
        return None;
    }
    let at = pts
        .iter()
        .position(|q| q.x > p.x)
        .unwrap_or(pts.len() - 1)
        .clamp(1, pts.len() - 1);
    let (lo, hi) = (pts[at - 1].x + EPS, pts[at].x - EPS);
    if lo > hi {
        return None;
    }
    let mut next = pts.to_vec();
    next.insert(
        at,
        CurvePoint {
            x: p.x.clamp(lo, hi),
            y: p.y.clamp(0.0, 1.0),
        },
    );
    Some((next, at))
}

/// `pts` with point `idx` moved: endpoints keep their x; interior points stay strictly between
/// their neighbours; y is clamped to `[0, 1]`.
fn moved(pts: &[CurvePoint], idx: usize, x: f32, y: f32) -> Vec<CurvePoint> {
    let mut next = pts.to_vec();
    let end = idx == 0 || idx + 1 == pts.len();
    let x = if end {
        pts[idx].x
    } else {
        let (lo, hi) = (pts[idx - 1].x + EPS, pts[idx + 1].x - EPS);
        x.clamp(lo.min(hi), hi.max(lo))
    };
    next[idx] = CurvePoint {
        x,
        y: y.clamp(0.0, 1.0),
    };
    next
}

/// `pts` without interior point `idx`; endpoints cannot be removed.
fn removed(pts: &[CurvePoint], idx: usize) -> Option<Vec<CurvePoint>> {
    if idx == 0 || idx + 1 >= pts.len() {
        return None;
    }
    let mut next = pts.to_vec();
    next.remove(idx);
    Some(next)
}

/// The channel's curve as 256 output values, from core-model's LUT (what the GPU applies).
fn samples(pts: &[CurvePoint], ch: usize) -> Vec<f32> {
    let mut tc = ToneCurve::default();
    *channel_mut(&mut tc, ch) = pts.to_vec();
    let lut = build_lut(&tc);
    let component = ch.saturating_sub(1).min(2);
    (0..256)
        .map(|i| f32::from(lut[i * 4 + component]) / 255.0)
        .collect()
}

struct Plot {
    rect: Rect,
}

/// What an edit needs besides the pointer: the plot geometry, the document's params and the
/// active channel.
struct Ctx<'a> {
    plot: Plot,
    base: &'a DevelopParams,
    ch: usize,
}

impl Plot {
    fn to_screen(&self, p: CurvePoint) -> Pos2 {
        Pos2::new(
            self.rect.left() + p.x * self.rect.width(),
            self.rect.bottom() - p.y * self.rect.height(),
        )
    }

    fn to_value(&self, pos: Pos2) -> (f32, f32) {
        (
            ((pos.x - self.rect.left()) / self.rect.width()).clamp(0.0, 1.0),
            (1.0 - (pos.y - self.rect.top()) / self.rect.height()).clamp(0.0, 1.0),
        )
    }

    fn hit(&self, pts: &[CurvePoint], pos: Pos2) -> Option<usize> {
        pts.iter()
            .position(|p| self.to_screen(*p).distance(pos) <= HIT_RADIUS)
    }
}

fn paint_plot(ui: &Ui, plot: &Plot, pts: &[CurvePoint], ch: usize, env: &Env<'_>) {
    let painter = ui.painter();
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let x = plot.rect.left() + plot.rect.width() * t;
        let y = plot.rect.top() + plot.rect.height() * t;
        painter.vline(x, plot.rect.y_range(), Stroke::new(1.0, theme::LINE));
        painter.hline(plot.rect.x_range(), y, Stroke::new(1.0, theme::LINE));
    }
    if let Some(h) = env.histogram {
        let lum: Vec<u32> =
            h.r.iter()
                .zip(&h.g)
                .zip(&h.b)
                .map(|((r, g), b)| (r + g + b) / 3)
                .collect();
        let peak = histogram::scale_max([&lum, &lum, &lum]);
        painter.add(histogram::area_mesh(
            &lum,
            peak,
            plot.rect,
            Color32::from_white_alpha(18),
        ));
    }
    let line: Vec<Pos2> = samples(pts, ch)
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            plot.to_screen(CurvePoint {
                x: i as f32 / 255.0,
                y: v,
            })
        })
        .collect();
    painter.add(egui::Shape::line(line, Stroke::new(1.6, channel_color(ch))));
    for (i, p) in pts.iter().enumerate() {
        let end = i == 0 || i + 1 == pts.len();
        let c = plot.to_screen(*p);
        painter.circle_filled(c, 5.0, theme::STAGE_DEV);
        painter.circle_filled(c, 3.5, if end { theme::T3 } else { theme::ACCENT });
    }
}

/// The curve of `params` with channel `ch` replaced by `pts`.
fn with_points(params: &DevelopParams, ch: usize, pts: Vec<CurvePoint>) -> DevelopParams {
    let mut next = params.clone();
    *channel_mut(&mut next.tone_curve, ch) = pts;
    next
}

pub(super) fn tone_curve(ui: &mut Ui, env: &mut Env<'_>, st: &mut DevelopUi) {
    let side = ui.available_width();
    let live = env.enabled && env.params.is_some();
    let sense = if live {
        Sense::click_and_drag()
    } else {
        Sense::hover()
    };
    let (outer, response) = ui.allocate_exact_size(egui::vec2(side, side), sense);
    ui.painter()
        .rect_filled(outer, theme::RADIUS_SM, theme::STAGE_DEV);
    ui.painter().rect_stroke(
        outer,
        theme::RADIUS_SM,
        Stroke::new(1.0, theme::LINE),
        egui::StrokeKind::Inside,
    );
    let plot = Plot {
        rect: outer.shrink(PAD),
    };
    st.curve_rect = Some(plot.rect);
    let ch = st.curve_channel.min(3);
    let current = env.params.map(|p| points_of(&p.tone_curve, ch));
    let pts = current
        .clone()
        .unwrap_or_else(|| vec![CurvePoint { x: 0.0, y: 0.0 }, CurvePoint { x: 1.0, y: 1.0 }]);
    paint_plot(ui, &plot, &pts, ch, env);
    if let (true, Some(base), Some(current)) = (live, env.params, current) {
        let response = response.on_hover_cursor(CursorIcon::Crosshair);
        let ctx = Ctx { plot, base, ch };
        edit(ui, &response, &ctx, current, env, st);
    } else if st.curve_grab.is_some() {
        st.curve_grab = None;
    }
    tabs(ui, st);
}

/// Pointer handling: drag a point = gesture, press-drag on empty plot inserts and drags a new
/// point, plain click on empty plot inserts (Apply), double-click removes an interior point.
fn edit(
    ui: &Ui,
    response: &egui::Response,
    ctx: &Ctx<'_>,
    pts: Vec<CurvePoint>,
    env: &mut Env<'_>,
    st: &mut DevelopUi,
) {
    let Ctx { plot, base, ch } = ctx;
    let id: Id = response.id;
    if response.is_pointer_button_down_on() && st.curve_grab.is_none() {
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .unwrap_or(plot.rect.center());
        st.curve_grab = Some(Grab {
            index: plot.hit(&pts, origin),
            last: None,
            base: pts.clone(),
        });
    }
    if response.drag_started() && st.gesture.is_none() {
        if let (Some(mut grab), Some(pos)) =
            (st.curve_grab.clone(), response.interact_pointer_pos())
        {
            if grab.index.is_none() {
                let (x, y) = plot.to_value(pos);
                if let Some((next, at)) = inserted(&pts, CurvePoint { x, y }) {
                    grab.base = next;
                    grab.index = Some(at);
                }
            }
            if grab.index.is_some() {
                st.cancelled = false;
                st.gesture = Some(id);
                env.intent(EditIntent::BeginGesture);
            }
            st.curve_grab = Some(grab);
        }
    }
    if st.gesture == Some(id) && ui.input(|i| i.key_pressed(Key::Escape)) {
        st.gesture = None;
        st.cancelled = true;
        env.intent(EditIntent::CancelGesture);
    }
    if response.dragged() && st.gesture == Some(id) {
        drag_update(response, ctx, env, st);
    }
    if response.drag_stopped() {
        if st.gesture == Some(id) {
            st.gesture = None;
            env.intent(EditIntent::CommitGesture);
        }
        st.cancelled = false;
    }
    let pos = response.interact_pointer_pos();
    if response.double_clicked() {
        if let Some(next) = pos
            .and_then(|p| plot.hit(&pts, p))
            .and_then(|i| removed(&pts, i))
        {
            env.apply(with_points(base, *ch, next));
        }
    } else if response.clicked() {
        let landed_on_point = st.curve_grab.as_ref().is_some_and(|g| g.index.is_some());
        if let (false, Some(pos)) = (landed_on_point, pos) {
            let (x, y) = plot.to_value(pos);
            if let Some((next, _)) = inserted(&pts, CurvePoint { x, y }) {
                env.apply(with_points(base, *ch, next));
            }
        }
    }
    if !response.is_pointer_button_down_on() && !response.dragged() {
        st.curve_grab = None;
    }
}

fn drag_update(response: &egui::Response, ctx: &Ctx<'_>, env: &mut Env<'_>, st: &mut DevelopUi) {
    let (Some(grab), Some(pos)) = (st.curve_grab.as_mut(), response.interact_pointer_pos()) else {
        return;
    };
    let Some(index) = grab.index else { return };
    let (x, y) = ctx.plot.to_value(pos);
    if grab.last == Some((x, y)) || index >= grab.base.len() {
        return;
    }
    grab.last = Some((x, y));
    let next = moved(&grab.base, index, x, y);
    env.intent(EditIntent::UpdateGesture(Box::new(with_points(
        ctx.base, ctx.ch, next,
    ))));
}

fn tabs(ui: &mut Ui, st: &mut DevelopUi) {
    let gap = 2.0;
    let width = (ui.available_width() - gap * 3.0) / 4.0;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, label) in TABS.iter().enumerate() {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 22.0), Sense::click());
            let on = st.curve_channel == i;
            if on {
                ui.painter().rect_filled(rect, 4, theme::ELEV);
            } else if response.hovered() {
                ui.painter()
                    .rect_filled(rect, 4, theme::HOVER.gamma_multiply(0.5));
            }
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                FontId::proportional(11.0),
                if on { theme::T1 } else { theme::T3 },
            );
            if response.clicked() {
                st.curve_channel = i;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> CurvePoint {
        CurvePoint { x, y }
    }

    #[test]
    fn identity_samples_are_a_straight_ramp() {
        let s = samples(&[pt(0.0, 0.0), pt(1.0, 1.0)], 0);
        for (i, v) in s.iter().enumerate() {
            assert!((v - i as f32 / 255.0).abs() < 1e-6);
        }
    }

    #[test]
    fn samples_pass_through_the_control_points_of_the_chosen_channel() {
        let pts = [pt(0.0, 0.0), pt(0.5, 0.75), pt(1.0, 1.0)];
        for ch in 0..4 {
            let s = samples(&pts, ch);
            assert!((s[128] - 0.75).abs() < 0.02, "channel {ch}: {}", s[128]);
        }
    }

    #[test]
    fn insert_keeps_x_sorted_between_the_endpoints() {
        let base = [pt(0.0, 0.0), pt(1.0, 1.0)];
        let (next, at) = inserted(&base, pt(0.4, 0.7)).unwrap();
        assert_eq!(at, 1);
        assert_eq!(next.len(), 3);
        assert!(next.windows(2).all(|w| w[0].x < w[1].x));
        // Past either end it is still an interior point.
        assert_eq!(inserted(&base, pt(1.0, 0.2)).unwrap().0.len(), 3);
        assert!(inserted(&base, pt(1.0, 0.2)).unwrap().0[1].x < 1.0);
    }

    #[test]
    fn insert_stops_at_the_point_limit() {
        let full: Vec<CurvePoint> = (0..MAX_POINTS)
            .map(|i| pt(i as f32 / (MAX_POINTS - 1) as f32, 0.5))
            .collect();
        assert!(inserted(&full, pt(0.51, 0.2)).is_none());
    }

    #[test]
    fn move_pins_endpoint_x_and_clamps_interior_between_neighbours() {
        let pts = [pt(0.0, 0.0), pt(0.4, 0.5), pt(0.7, 0.6), pt(1.0, 1.0)];
        let m = moved(&pts, 0, 0.5, -3.0);
        assert_eq!((m[0].x, m[0].y), (0.0, 0.0));
        let m = moved(&pts, 1, 0.99, 2.0);
        assert!(m[1].x < m[2].x && m[1].x > 0.69);
        assert_eq!(m[1].y, 1.0);
        let m = moved(&pts, 2, -1.0, 0.2);
        assert!(m[2].x > m[1].x);
        assert!(m.windows(2).all(|w| w[0].x < w[1].x));
    }

    #[test]
    fn only_interior_points_can_be_removed() {
        let pts = [pt(0.0, 0.0), pt(0.5, 0.6), pt(1.0, 1.0)];
        assert!(removed(&pts, 0).is_none());
        assert!(removed(&pts, 2).is_none());
        assert_eq!(removed(&pts, 1).unwrap().len(), 2);
    }
}
