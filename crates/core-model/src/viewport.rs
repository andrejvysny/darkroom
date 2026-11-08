//! Pure viewport zoom/pan geometry for the Develop stage, ported from the legacy
//! `src/lib/viewport.ts` (`deriveViewRect`, `zoomAtPoint`, `canvasPxToNorm`, `zoom1to1`).
//!
//! Everything works in PHYSICAL pixels — there is no CSS/DPR concept; callers pass
//! physical-pixel sizes. The view lives in crop-local UV space `[0,1]²`: `origin` is the
//! top-left corner of the visible window and `size` its extent, both in crop-local UV.
//! Zoom is relative to fit (1.0 = whole crop visible).

/// Zoom floor: fit. Native does not zoom out below fit.
pub const MIN_ZOOM: f64 = 1.0;
/// The ceiling in actual pixels: 400 % is enough to peep at pixels, and a bigger render target
/// would only cost memory.
pub const MAX_ACTUAL_ZOOM: f64 = 4.0;

/// Zoom and pan state: zoom relative to fit, and the view centre in crop-local UV `[0,1]²`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewState {
    pub zoom: f64,
    pub center: [f64; 2],
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: MIN_ZOOM,
            center: [0.5, 0.5],
        }
    }
}

impl ViewState {
    /// True when the view is (at or below) fit.
    pub fn is_fit(&self) -> bool {
        self.zoom <= MIN_ZOOM + 1e-9
    }
}

/// Derived render geometry from a [`ViewState`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DerivedView {
    /// Top-left of the visible window, crop-local UV.
    pub origin: [f64; 2],
    /// Window size, crop-local UV, each in `(0, 1]`.
    pub size: [f64; 2],
    /// Render target = visible image area in physical px, each >= 1.
    pub out_px: (u32, u32),
    /// On-screen size of the WHOLE image at this zoom, physical px.
    pub image_px: [f64; 2],
}

/// `frame_px` = rendered frame (cropped output) pixel dims; `panel_px` = stage size in px.
pub fn derive_view(state: ViewState, frame_px: (u32, u32), panel_px: (u32, u32)) -> DerivedView {
    let zoom = clamp_zoom(state.zoom, frame_px, panel_px);
    let image_px = image_size(zoom, frame_px, panel_px);
    let size = window_size(image_px, panel_px);
    // Centre clamp: at least half a window from each edge so the window stays within [0,1].
    let center = [
        clamp_center(finite(state.center[0], 0.5), size[0] / 2.0),
        clamp_center(finite(state.center[1], 0.5), size[1] / 2.0),
    ];
    DerivedView {
        origin: [center[0] - size[0] / 2.0, center[1] - size[1] / 2.0],
        size,
        out_px: out_px(image_px, panel_px),
        image_px,
    }
}

/// Largest zoom allowed: 400 % of actual pixels, never below fit — a tiny image (or tight crop)
/// that already fits above 400 % simply cannot zoom further, and the clamp bounds never invert.
pub fn max_zoom(frame_px: (u32, u32), panel_px: (u32, u32)) -> f64 {
    MIN_ZOOM.max(MAX_ACTUAL_ZOOM * one_to_one_zoom(frame_px, panel_px))
}

/// Zoom at which one frame pixel == one physical panel pixel.
pub fn one_to_one_zoom(frame_px: (u32, u32), panel_px: (u32, u32)) -> f64 {
    let (fw, fh) = clamped_px(frame_px);
    let (pw, ph) = clamped_px(panel_px);
    1.0 / (pw / fw).min(ph / fh)
}

/// Zoom to `next_zoom` (clamped to `[MIN_ZOOM, max_zoom]`) keeping the image point under
/// `anchor_px` (relative to the TOP-LEFT of the visible image area) fixed on screen.
pub fn zoom_at(
    state: ViewState,
    next_zoom: f64,
    anchor_px: [f64; 2],
    frame_px: (u32, u32),
    panel_px: (u32, u32),
) -> ViewState {
    if !next_zoom.is_finite() || !finite_pair(anchor_px) {
        return clamped_state(state, frame_px, panel_px);
    }
    let zoom = clamp_zoom(next_zoom, frame_px, panel_px);
    let before = derive_view(state, frame_px, panel_px);
    // What crop-local UV point is under the cursor now...
    let uv = screen_to_uv(&before, anchor_px);
    // ...must land at the same screen fraction of the (newly sized) visible area afterwards.
    let after_image = image_size(zoom, frame_px, panel_px);
    let after_size = window_size(after_image, panel_px);
    let frac = screen_frac(&before, anchor_px);
    let center = [
        uv[0] - (frac[0] - 0.5) * after_size[0],
        uv[1] - (frac[1] - 0.5) * after_size[1],
    ];
    clamped_state(ViewState { zoom, center }, frame_px, panel_px)
}

/// Pan by a screen delta in physical px (drag right ⇒ image moves right ⇒ centre moves left).
pub fn pan_by(
    state: ViewState,
    delta_px: [f64; 2],
    frame_px: (u32, u32),
    panel_px: (u32, u32),
) -> ViewState {
    if !finite_pair(delta_px) {
        return clamped_state(state, frame_px, panel_px);
    }
    let zoom = clamp_zoom(state.zoom, frame_px, panel_px);
    let image_px = image_size(zoom, frame_px, panel_px);
    clamped_state(
        ViewState {
            zoom,
            center: [
                finite(state.center[0], 0.5) - delta_px[0] / image_px[0],
                finite(state.center[1], 0.5) - delta_px[1] / image_px[1],
            ],
        },
        frame_px,
        panel_px,
    )
}

/// Crop-local UV of a point given relative to the top-left of the visible image area.
pub fn screen_to_uv(view: &DerivedView, point_px: [f64; 2]) -> [f64; 2] {
    let frac = screen_frac(view, point_px);
    [
        view.origin[0] + frac[0] * view.size[0],
        view.origin[1] + frac[1] * view.size[1],
    ]
}

/// Physical px of the panel and the frame, each side clamped to >= 1.
fn clamped_px(px: (u32, u32)) -> (f64, f64) {
    (f64::from(px.0.max(1)), f64::from(px.1.max(1)))
}

/// On-screen size of the whole image at `zoom` (zoom = 1 ⇒ fit, letterboxed/pillarboxed).
fn image_size(zoom: f64, frame_px: (u32, u32), panel_px: (u32, u32)) -> [f64; 2] {
    let (fw, fh) = clamped_px(frame_px);
    let (pw, ph) = clamped_px(panel_px);
    let aspect = fw / fh;
    let (fit_w, fit_h) = if pw / ph > aspect {
        (ph * aspect, ph) // pillarbox — height fills
    } else {
        (pw, pw / aspect) // letterbox — width fills
    };
    [fit_w * zoom, fit_h * zoom]
}

/// Visible window in crop-local UV = the visible (image ∩ panel) fraction per axis.
fn window_size(image_px: [f64; 2], panel_px: (u32, u32)) -> [f64; 2] {
    let (pw, ph) = clamped_px(panel_px);
    [
        (pw.min(image_px[0]) / image_px[0]).min(1.0),
        (ph.min(image_px[1]) / image_px[1]).min(1.0),
    ]
}

/// Render target: the visible image area, rounded, each side >= 1.
fn out_px(image_px: [f64; 2], panel_px: (u32, u32)) -> (u32, u32) {
    let (pw, ph) = clamped_px(panel_px);
    (
        pw.min(image_px[0]).round().max(1.0) as u32,
        ph.min(image_px[1]).round().max(1.0) as u32,
    )
}

/// Screen fraction of a point within the visible image area (`size * image_px` is the exact
/// visible px extent: `size = min(1, visible / image_px)` and `visible <= image_px`).
fn screen_frac(view: &DerivedView, point_px: [f64; 2]) -> [f64; 2] {
    [
        finite(point_px[0], 0.0) / (view.size[0] * view.image_px[0]).max(1.0),
        finite(point_px[1], 0.0) / (view.size[1] * view.image_px[1]).max(1.0),
    ]
}

fn clamp_zoom(zoom: f64, frame_px: (u32, u32), panel_px: (u32, u32)) -> f64 {
    finite(zoom, MIN_ZOOM).clamp(MIN_ZOOM, max_zoom(frame_px, panel_px))
}

fn clamp_center(center: f64, half_size: f64) -> f64 {
    center.max(half_size).min(1.0 - half_size)
}

/// A state with finite zoom in `[MIN_ZOOM, max_zoom]` and a centre clamped into the window.
fn clamped_state(state: ViewState, frame_px: (u32, u32), panel_px: (u32, u32)) -> ViewState {
    let view = derive_view(state, frame_px, panel_px);
    ViewState {
        zoom: clamp_zoom(state.zoom, frame_px, panel_px),
        center: [
            view.origin[0] + view.size[0] / 2.0,
            view.origin[1] + view.size[1] / 2.0,
        ],
    }
}

fn finite(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

fn finite_pair(v: [f64; 2]) -> bool {
    v[0].is_finite() && v[1].is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: (u32, u32) = (3000, 2000); // 3:2
    const PANEL_SAME: (u32, u32) = (1200, 800); // same 3:2 aspect

    fn all_finite(d: &DerivedView) -> bool {
        d.origin.into_iter().all(f64::is_finite)
            && d.size.into_iter().all(f64::is_finite)
            && d.image_px.into_iter().all(f64::is_finite)
            && d.out_px.0 >= 1
            && d.out_px.1 >= 1
    }

    #[test]
    fn fit_matches_legacy_fit_in_wide_panel() {
        let d = derive_view(ViewState::default(), FRAME, (1600, 800));
        assert_eq!(d.origin, [0.0, 0.0]);
        assert_eq!(d.size, [1.0, 1.0]);
        // == ui_egui::photo::fit_output((1600, 800), 1.5): height-bound, 800 * 1.5.
        assert_eq!(d.out_px, (1200, 800));
        assert_eq!(d.image_px, [1200.0, 800.0]);
    }

    #[test]
    fn fit_matches_legacy_fit_in_tall_panel() {
        let d = derive_view(ViewState::default(), FRAME, (800, 1000));
        assert_eq!(d.origin, [0.0, 0.0]);
        assert_eq!(d.size, [1.0, 1.0]);
        // == fit_output((800, 1000), 1.5): width-bound, round(800 / 1.5) = 533.
        assert_eq!(d.out_px, (800, 533));
    }

    #[test]
    fn zoom_two_in_aspect_matched_panel() {
        let d = derive_view(
            ViewState {
                zoom: 2.0,
                center: [0.5, 0.5],
            },
            FRAME,
            PANEL_SAME,
        );
        assert_eq!(d.size, [0.5, 0.5]);
        assert_eq!(d.out_px, PANEL_SAME);
        assert_eq!(d.origin, [0.25, 0.25]);
    }

    #[test]
    fn centre_clamps_at_corners() {
        let d = derive_view(
            ViewState {
                zoom: 2.0,
                center: [0.0, 0.0],
            },
            FRAME,
            PANEL_SAME,
        );
        assert_eq!(d.origin, [0.0, 0.0]);
        assert_eq!(d.size, [0.5, 0.5]);
        let d = derive_view(
            ViewState {
                zoom: 2.0,
                center: [1.0, 1.0],
            },
            FRAME,
            PANEL_SAME,
        );
        assert_eq!(d.origin, [0.5, 0.5]);
    }

    #[test]
    fn zoom_at_keeps_anchor_point_fixed() {
        for anchor in [[300.0, 200.0], [900.0, 600.0], [1150.0, 100.0]] {
            let before = derive_view(ViewState::default(), FRAME, PANEL_SAME);
            let after = derive_view(
                zoom_at(ViewState::default(), 2.0, anchor, FRAME, PANEL_SAME),
                FRAME,
                PANEL_SAME,
            );
            let u0 = screen_to_uv(&before, anchor);
            let u1 = screen_to_uv(&after, anchor);
            assert!(
                (u0[0] - u1[0]).abs() < 1e-9 && (u0[1] - u1[1]).abs() < 1e-9,
                "anchor {anchor:?}: {u0:?} vs {u1:?}"
            );
        }
    }

    #[test]
    fn zoom_at_clamps_to_limits() {
        let z = zoom_at(ViewState::default(), 0.1, [0.0, 0.0], FRAME, PANEL_SAME);
        assert_eq!(z.zoom, MIN_ZOOM);
        let z = zoom_at(ViewState::default(), 1e9, [0.0, 0.0], FRAME, PANEL_SAME);
        assert_eq!(z.zoom, max_zoom(FRAME, PANEL_SAME));
        assert!(z.center.into_iter().all(f64::is_finite));
    }

    #[test]
    fn max_zoom_is_four_times_one_to_one_for_a_big_image() {
        for (frame, panel) in [
            ((10000u32, 10000u32), (1200u32, 800u32)),
            ((6960, 4640), (1392, 928)),
            ((4640, 6960), (800, 1000)), // portrait photo, portrait-ish panel
            ((6960, 4640), (500, 1400)), // portrait panel, landscape photo
        ] {
            let o = one_to_one_zoom(frame, panel);
            assert!(o > 1.0);
            assert!((max_zoom(frame, panel) - 4.0 * o).abs() < 1e-9);
        }
    }

    #[test]
    fn max_zoom_never_drops_below_fit() {
        // A tiny image (already shown above 100 % at fit) and a tight crop of one: the ceiling is
        // fit itself, not a value below it.
        assert_eq!(max_zoom((200, 100), (1200, 800)), MIN_ZOOM);
        assert_eq!(max_zoom((1, 1), (1200, 800)), MIN_ZOOM);
        let z = zoom_at(
            ViewState::default(),
            3.0,
            [10.0, 10.0],
            (200, 100),
            (1200, 800),
        );
        assert_eq!(z.zoom, MIN_ZOOM);
    }

    #[test]
    fn one_to_one_zoom_known_value() {
        // 6960x4640 frame in a 1392x928 panel: panel is exactly 1/5 of the frame.
        let z = one_to_one_zoom((6960, 4640), (1392, 928));
        assert!((z - 5.0).abs() < 1e-12, "{z}");
    }

    #[test]
    fn pan_by_moves_centre_opposite_and_clamps() {
        let zoomed = ViewState {
            zoom: 2.0,
            center: [0.5, 0.5],
        };
        // image_px = 2400x1600; a 240 px drag right moves the centre left by 0.1 UV.
        let p = pan_by(zoomed, [240.0, 0.0], FRAME, PANEL_SAME);
        assert!((p.center[0] - 0.4).abs() < 1e-12);
        assert!((p.center[1] - 0.5).abs() < 1e-12);
        // Clamps at the left edge (centre >= size/2 = 0.25).
        let p = pan_by(
            ViewState {
                zoom: 2.0,
                center: [0.3, 0.5],
            },
            [240.0, 0.0],
            FRAME,
            PANEL_SAME,
        );
        assert_eq!(p.center, [0.25, 0.5]);
    }

    #[test]
    fn pan_at_fit_is_a_no_op() {
        let p = pan_by(ViewState::default(), [500.0, 300.0], FRAME, PANEL_SAME);
        assert_eq!(p, ViewState::default());
    }

    #[test]
    fn nan_inputs_give_finite_output_without_panicking() {
        let nan = f64::NAN;
        let d = derive_view(
            ViewState {
                zoom: nan,
                center: [nan, 0.2],
            },
            FRAME,
            PANEL_SAME,
        );
        assert!(all_finite(&d));
        assert_eq!(d, derive_view(ViewState::default(), FRAME, PANEL_SAME)); // zoom fell back to fit

        let z = zoom_at(ViewState::default(), nan, [nan, 0.0], FRAME, PANEL_SAME);
        assert_eq!(z, ViewState::default());
        let z = zoom_at(ViewState::default(), 2.0, [nan, 0.0], FRAME, PANEL_SAME);
        assert_eq!(z, ViewState::default());

        let p = pan_by(
            ViewState {
                zoom: 2.0,
                center: [0.5, 0.5],
            },
            [nan, 0.0],
            FRAME,
            PANEL_SAME,
        );
        assert!(p.center.into_iter().all(f64::is_finite) && p.zoom.is_finite());
    }
}
