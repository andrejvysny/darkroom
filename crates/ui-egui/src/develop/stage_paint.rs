//! The stage compositor: paints a cached placeholder under the live frame, re-projects a frame
//! rendered for another view window into the current one, fades the live frame in over the
//! placeholder, and shows the "work pending" dot. Pure geometry here is unit-tested without egui
//! contexts; the stateful parts keep their memory in the egui context.

use crate::photo::Picture;
use crate::theme;
use core_model::identity::ImageId;
use egui::{pos2, Color32, Rect, TextureId};

/// A crop-local UV window: `(origin, size)`.
pub(crate) type Win = ([f32; 2], [f32; 2]);

const WHOLE: Win = ([0.0, 0.0], [1.0, 1.0]);
/// Placeholder -> live crossfade.
pub(crate) const FADE_SECS: f32 = 0.12;
/// Pending work shows its dot only after this long, so a fast render never flashes it.
const DOT_DELAY_SECS: f64 = 0.25;

/// Where a picture showing `pic` (None = whole frame) lands inside `dest`, which is the screen
/// rect of the CURRENT view window `view` (None = whole frame): the part of both windows they
/// share, as `(screen rect, uv rect into the picture)`. `None` when they share nothing. The part
/// of `dest` outside the shared area is left to whatever is painted beneath.
pub(crate) fn reproject(pic: Option<Win>, view: Option<Win>, dest: Rect) -> Option<(Rect, Rect)> {
    let (po, ps) = pic.unwrap_or(WHOLE);
    let (vo, vs) = view.unwrap_or(WHOLE);
    if ps[0] <= 0.0 || ps[1] <= 0.0 || vs[0] <= 0.0 || vs[1] <= 0.0 {
        return None;
    }
    // Per axis: the overlap of [po, po+ps] and [vo, vo+vs] as fractions of the view (screen) and
    // of the picture (uv).
    let axis = |a: usize| {
        let lo = po[a].max(vo[a]);
        let hi = (po[a] + ps[a]).min(vo[a] + vs[a]);
        (hi > lo).then(|| {
            (
                (lo - vo[a]) / vs[a],
                (hi - vo[a]) / vs[a],
                (lo - po[a]) / ps[a],
                (hi - po[a]) / ps[a],
            )
        })
    };
    let (x, y) = (axis(0)?, axis(1)?);
    let (w, h) = (dest.width(), dest.height());
    let screen = Rect::from_min_max(
        pos2(dest.min.x + x.0 * w, dest.min.y + y.0 * h),
        pos2(dest.min.x + x.1 * w, dest.min.y + y.1 * h),
    );
    Some((screen, Rect::from_min_max(pos2(x.2, y.2), pos2(x.3, y.3))))
}

fn is_identity(screen: Rect, uv: Rect, dest: Rect) -> bool {
    let eps = 1e-3;
    (screen.min - dest.min).abs().max_elem() < eps
        && (screen.max - dest.max).abs().max_elem() < eps
        && (uv.min.to_vec2().abs().max_elem()) < 1e-4
        && ((uv.max - pos2(1.0, 1.0)).abs().max_elem()) < 1e-4
}

fn paint_picture(
    painter: &egui::Painter,
    pic: &Picture,
    view: Option<Win>,
    dest: Rect,
    tint: Color32,
) {
    if let Some((screen, uv)) = reproject(pic.window, view, dest) {
        painter.image(pic.texture, screen, uv, tint);
    }
}

/// The soft shadow, the placeholder (when given) and the live frame over it, both re-projected
/// into `dest` (the current view's screen rect). `alpha` is the live frame's fade.
pub(crate) fn paint(
    ui: &egui::Ui,
    dest: Rect,
    backdrop: Option<&Picture>,
    live: Option<&Picture>,
    view: Option<Win>,
    alpha: f32,
) {
    let painter = ui.painter();
    let shadow = egui::Shadow {
        offset: [0, 6],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(150),
    };
    painter.add(shadow.as_shape(dest, 0));
    let live_covers = live.is_some_and(|pic| {
        alpha >= 1.0
            && reproject(pic.window, view, dest)
                .is_some_and(|(screen, uv)| is_identity(screen, uv, dest))
    });
    if let (Some(pic), false) = (backdrop, live_covers) {
        paint_picture(painter, pic, view, dest, Color32::WHITE);
    }
    if let Some(pic) = live {
        let tint = Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0).round() as u8);
        paint_picture(painter, pic, view, dest, tint);
    }
}

/// Whether a freshly arrived live frame fades in: only over a placeholder that was on screen
/// (nothing to blend with otherwise) and never while the user is mid zoom/pan gesture.
pub(crate) fn fade_starts(
    had_live: bool,
    backdrop_was_shown: bool,
    new_live: bool,
    interacting: bool,
) -> bool {
    new_live && !had_live && backdrop_was_shown && !interacting
}

#[derive(Clone, Default)]
struct Memory {
    image: Option<ImageId>,
    live: Option<TextureId>,
    backdrop_shown: bool,
    fades: u64,
    fading: bool,
}

/// The live frame's opacity this frame (1.0 except during a placeholder -> live crossfade).
/// `backdrop_painted`: a placeholder is on screen this frame without a live frame.
pub(crate) fn live_alpha(
    ctx: &egui::Context,
    image: Option<ImageId>,
    live: Option<TextureId>,
    backdrop_painted: bool,
    interacting: bool,
) -> f32 {
    let key = egui::Id::unique("stage_present_memory");
    let mut mem: Memory = ctx.data(|d| d.get_temp(key)).unwrap_or_default();
    if mem.image != image {
        mem = Memory {
            image,
            ..Memory::default()
        };
    }
    let new_live = live.is_some() && mem.live != live;
    let starts = fade_starts(
        mem.live.is_some(),
        mem.backdrop_shown,
        new_live,
        interacting,
    );
    if starts {
        mem.fades += 1;
        mem.fading = true;
        // Seed the animation at 0 so the retarget below runs 0 -> 1 (the first call of an id
        // returns its target without animating).
        ctx.animate_value_with_time(egui::Id::unique(("stage_fade", mem.fades)), 0.0, FADE_SECS);
    }
    let mut alpha = 1.0;
    if mem.fading && live.is_some() {
        let id = egui::Id::unique(("stage_fade", mem.fades));
        alpha = ctx.animate_value_with_time(id, 1.0, FADE_SECS);
        mem.fading = alpha < 1.0;
    }
    mem.live = live;
    mem.backdrop_shown = live.is_none() && backdrop_painted;
    ctx.data_mut(|d| d.insert_temp(key, mem));
    alpha
}

/// A small dot in the stage's top-right corner while work is pending for longer than
/// [`DOT_DELAY_SECS`]; unobtrusive replacement for the centered spinner once something is painted.
pub(crate) fn activity_dot(ui: &egui::Ui, stage: Rect, pending: bool) {
    let key = egui::Id::unique("stage_pending_since");
    if !pending {
        ui.ctx().data_mut(|d| d.remove_temp::<f64>(key));
        return;
    }
    let now = ui.input(|i| i.time);
    let since = ui
        .ctx()
        .data_mut(|d| *d.get_temp_mut_or_insert_with(key, || now));
    let waited = now - since;
    if waited < DOT_DELAY_SECS {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(DOT_DELAY_SECS - waited));
        return;
    }
    // Gentle pulse; keeps the stage animating only while pending.
    let pulse = 0.55 + 0.45 * (0.5 + 0.5 * (now * 4.0).sin()) as f32;
    ui.painter().circle_filled(
        stage.right_top() + egui::vec2(-16.0, 16.0),
        5.0,
        theme::T2.gamma_multiply(pulse),
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(50));
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEST: fn() -> Rect = || Rect::from_min_max(pos2(100.0, 50.0), pos2(500.0, 350.0));
    fn win(o: [f32; 2], s: [f32; 2]) -> Win {
        (o, s)
    }

    #[test]
    fn identity_at_fit_and_for_equal_windows() {
        let (screen, uv) = reproject(None, None, DEST()).unwrap();
        assert!(is_identity(screen, uv, DEST()));
        let w = win([0.2, 0.3], [0.4, 0.4]);
        let (screen, uv) = reproject(Some(w), Some(w), DEST()).unwrap();
        assert!(is_identity(screen, uv, DEST()));
    }

    #[test]
    fn zooming_in_samples_the_subwindow_of_a_whole_frame() {
        // Whole-frame picture, view zoomed to the centre quarter: fill dest, sample uv 0.25..0.75.
        let (screen, uv) = reproject(None, Some(win([0.25, 0.25], [0.5, 0.5])), DEST()).unwrap();
        assert_eq!(screen, DEST());
        assert!((uv.min.x - 0.25).abs() < 1e-6 && (uv.max.y - 0.75).abs() < 1e-6);
    }

    #[test]
    fn zooming_out_shrinks_the_old_window_into_the_new_view() {
        // Picture shows the centre half; the view is the whole frame: it lands in the middle.
        let (screen, uv) = reproject(Some(win([0.25, 0.25], [0.5, 0.5])), None, DEST()).unwrap();
        assert!((screen.min.x - 200.0).abs() < 1e-3 && (screen.max.x - 400.0).abs() < 1e-3);
        assert!((screen.min.y - 125.0).abs() < 1e-3 && (screen.max.y - 275.0).abs() < 1e-3);
        assert_eq!((uv.min, uv.max), (pos2(0.0, 0.0), pos2(1.0, 1.0)));
    }

    #[test]
    fn panning_past_the_old_window_clamps_and_leaves_the_rest_uncovered() {
        // Picture [0, 0.5); view [0.25, 0.75): only the left half of the view is covered.
        let (screen, uv) = reproject(
            Some(win([0.0, 0.0], [0.5, 1.0])),
            Some(win([0.25, 0.0], [0.5, 1.0])),
            DEST(),
        )
        .unwrap();
        assert!((screen.min.x - 100.0).abs() < 1e-3 && (screen.max.x - 300.0).abs() < 1e-3);
        assert!((uv.min.x - 0.5).abs() < 1e-6 && (uv.max.x - 1.0).abs() < 1e-6);
        assert!(screen.width() < DEST().width());
    }

    #[test]
    fn disjoint_windows_paint_nothing() {
        assert!(reproject(
            Some(win([0.0, 0.0], [0.2, 0.2])),
            Some(win([0.5, 0.5], [0.2, 0.2])),
            DEST()
        )
        .is_none());
    }

    #[test]
    fn fade_only_over_a_shown_placeholder_and_never_while_interacting() {
        assert!(fade_starts(false, true, true, false));
        assert!(
            !fade_starts(false, true, true, true),
            "zoom gesture in flight"
        );
        assert!(
            !fade_starts(false, false, true, false),
            "nothing to blend with"
        );
        assert!(
            !fade_starts(true, true, true, false),
            "live replaces live: no fade"
        );
        assert!(!fade_starts(false, true, false, false));
    }

    fn frame(ctx: &egui::Context, time: f64, run: impl FnOnce(&egui::Context)) {
        let input = egui::RawInput {
            time: Some(time),
            ..Default::default()
        };
        let mut run = Some(run);
        let mut output = ctx.run_ui(input, |ui| (run.take().unwrap())(ui.ctx()));
        output.textures_delta.clear();
    }

    #[test]
    fn live_frame_fades_in_over_the_placeholder_then_settles() {
        let ctx = egui::Context::default();
        let image = Some(ImageId(1));
        let tex = TextureId::Managed(9);
        // Frame 1: placeholder alone is painted.
        frame(&ctx, 0.0, |c| {
            assert_eq!(live_alpha(c, image, None, true, false), 1.0);
        });
        // Frame 2: the live frame arrives -> starts transparent.
        let mut first = 1.0;
        frame(&ctx, 0.016, |c| {
            first = live_alpha(c, image, Some(tex), false, false)
        });
        assert!(first < 0.5, "starts faded out, got {first}");
        // Mid-fade then done after FADE_SECS.
        let mut mid = 0.0;
        frame(&ctx, 0.016 + 0.06, |c| {
            mid = live_alpha(c, image, Some(tex), false, false)
        });
        assert!(mid > first && mid < 1.0, "mid {mid}");
        let mut end = 0.0;
        frame(&ctx, 0.016 + 0.3, |c| {
            end = live_alpha(c, image, Some(tex), false, false)
        });
        assert_eq!(end, 1.0);
    }

    #[test]
    fn no_fade_while_interacting() {
        let ctx = egui::Context::default();
        let image = Some(ImageId(1));
        frame(&ctx, 0.0, |c| {
            live_alpha(c, image, None, true, true);
        });
        let mut a = 0.0;
        frame(&ctx, 0.016, |c| {
            a = live_alpha(c, image, Some(TextureId::Managed(9)), false, true)
        });
        assert_eq!(a, 1.0);
    }
}
