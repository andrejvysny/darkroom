//! The Develop stage's zoom/pan, driven through the real panel: synthetic egui input over the
//! drawn photo must change the render plan the stage asks for — the window it renders, its tier
//! and its key — and must never eat `Cmd+Z` (undo).

use core_app::import::{ImportPhase, ImportView};
use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::Command;
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, FrameKey, ImageId, ProcessVersion, QualityTier};
use core_model::DevelopParams;
use egui::{
    Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, Pos2, RawInput, Rect, TextureId,
    TouchPhase, Vec2,
};
use std::path::PathBuf;
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, ThumbSource, ThumbState, UiState};

/// The 3:2 fixture; the stage sizes the drawn frame from these catalog dims.
const FIXTURE: (i64, i64) = (6960, 4640);
const SCREEN: (f32, f32) = (1800.0, 1100.0);
/// A presented settled frame (the stage draws from the panel and the aspect, never the size).
const TEXTURE: [u32; 2] = [820, 547];

fn snapshot() -> ApplicationSnapshot {
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
            selected: Some(ImageId(7)),
            image: Some(ImageRow {
                id: 7,
                filename: "a.cr3".into(),
                width: Some(FIXTURE.0),
                height: Some(FIXTURE.1),
                ..Default::default()
            }),
            document: DocumentView {
                status: DocumentStatus::Ready,
                params: Some(Arc::new(DevelopParams::default())),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                can_undo: true,
                ..Default::default()
            },
            save: SaveStatus::default(),
            ..Default::default()
        },
    }
}

struct Nothing;

/// The Import dialog, open in front of the stage (its keys belong to the dialog).
fn open_dialog() -> ImportView {
    ImportView {
        open: true,
        source: Some(PathBuf::from("/src")),
        recursive: true,
        files: Arc::new(Vec::new()),
        dedup: (0, 0),
        phase: ImportPhase::Ready,
        generation: 1,
        ..Default::default()
    }
}

impl ThumbSource for Nothing {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

/// What the stage asked the host to render, in a form comparable across frames.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wanted {
    key: FrameKey,
    viewport_px: (u32, u32),
    display_px: (u32, u32),
    window: Option<([f32; 2], [f32; 2])>,
    quality: QualityTier,
}

impl Wanted {
    /// The crop-local origin of the rendered window; panics when nothing is zoomed.
    fn origin(&self) -> [f32; 2] {
        self.window.expect("a zoomed stage renders a window").0
    }
}

struct Harness {
    ctx: egui::Context,
    ui: UiState,
    photo: PhotoUi,
    snapshot: ApplicationSnapshot,
    thumbs: Nothing,
    /// The import dialog's view: `open` puts a modal in front of the stage.
    import: ImportView,
    time: f64,
    /// Seconds per frame (the real-time wheel-ease tests run at 60 Hz).
    dt: f64,
}

impl Harness {
    /// Warm frames first: the side panels need a layout pass before the stage's rect settles.
    fn new() -> Self {
        let mut harness = Self {
            ctx: egui::Context::default(),
            ui: UiState {
                mode: Mode::Develop,
                ..Default::default()
            },
            photo: PhotoUi {
                texture: Some(ui_egui::Picture::bare(TextureId::Managed(7), TEXTURE)),
                message: None,
                failure: None,
                retry: false,
                loading: false,
                device_epoch: Default::default(),
                plan: None,
                placeholder: None,
            },
            snapshot: snapshot(),
            thumbs: Nothing,
            import: core_app::import::ImportView::default(),
            time: 0.0,
            dt: 0.05,
        };
        harness.settle(4);
        harness
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<Command> {
        self.time += self.dt;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(SCREEN.0, SCREEN.1),
            )),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let mut error = None;
        let mut notices = Vec::new();
        let mut commands = Vec::new();
        let mut output = self.ctx.run_ui(input, |root| {
            commands = ui_egui::root(
                root,
                ui_egui::Frame {
                    snapshot: &self.snapshot,
                    reopen: &reopen,
                    command_error: &mut error,
                    photo: &mut self.photo,
                    settings_notices: &mut notices,
                    thumbs: &mut self.thumbs,
                    histogram: None,
                    import: &self.import,
                    export: &core_app::export::ExportView::default(),
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        commands
    }

    /// Idle frames — enough that a finished gesture settles (the interactive window is 0.2 s) and
    /// the full-quality frame is the one reported.
    fn settle(&mut self, frames: usize) -> Wanted {
        for _ in 0..frames {
            self.frame(Vec::new());
        }
        self.wanted()
    }

    fn wanted(&self) -> Wanted {
        let plan = self
            .photo
            .plan
            .as_ref()
            .expect("a Ready document with params plans a frame");
        Wanted {
            key: plan.key,
            viewport_px: plan.viewport_px,
            display_px: plan.display_px,
            window: plan.window,
            quality: plan.key.quality,
        }
    }

    /// The centre of the drawn photo — a gesture there is a gesture on the stage.
    fn photo_center(&mut self) -> Pos2 {
        self.frame(Vec::new());
        self.ui
            .develop
            .stage_image_rect()
            .expect("the stage drew the photo")
            .center()
    }

    /// Move the pointer over the photo and let egui register the hover (it hit-tests last
    /// frame's widget stack, so hovering takes a frame).
    fn hover_photo(&mut self) -> Pos2 {
        let at = self.photo_center();
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![Event::PointerMoved(at)]);
        at
    }

    /// One frame of a wheel gesture.
    fn wheel_frame(&mut self, at: Pos2, delta: f32, modifiers: Modifiers) {
        let mut events = vec![Event::PointerMoved(at)];
        if modifiers != Modifiers::NONE {
            events.push(Event::ModifiersChanged(modifiers));
        }
        events.push(Event::MouseWheel {
            unit: MouseWheelUnit::Point,
            delta: Vec2::new(0.0, delta),
            phase: TouchPhase::Move,
            source: MouseWheelSource::Unknown,
            modifiers,
        });
        self.frame(events);
    }

    /// A trackpad/wheel gesture: egui smooths the wheel, so a real gesture is a run of events,
    /// not one big jump.
    fn wheel(&mut self, at: Pos2, total: f32, modifiers: Modifiers) {
        for _ in 0..8 {
            self.wheel_frame(at, total / 8.0, modifiers);
        }
    }

    fn pinch(&mut self, at: Pos2, factor: f32) {
        self.frame(vec![Event::PointerMoved(at), Event::Zoom(factor)]);
    }

    fn press(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }]);
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> Vec<Command> {
        let mut events = Vec::new();
        if modifiers != Modifiers::NONE {
            events.push(Event::ModifiersChanged(modifiers));
        }
        events.push(Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
        self.frame(events)
    }

    /// Zoom into a window with a pinch, settled.
    fn zoom_in(&mut self, factor: f32) -> Wanted {
        let at = self.hover_photo();
        self.pinch(at, factor);
        self.settle(6)
    }
}

#[test]
fn the_stage_starts_at_fit_and_renders_the_whole_crop() {
    let harness = Harness::new();
    let fit = harness.wanted();
    assert_eq!(fit.window, None, "fit renders the whole crop");
    assert_eq!(fit.viewport_px, fit.display_px);
    assert_eq!(fit.quality, QualityTier::Settled);
}

#[test]
fn ctrl_scroll_over_the_photo_asks_for_a_window_then_settles() {
    let mut harness = Harness::new();
    let fit = harness.wanted();
    let at = harness.hover_photo();
    harness.wheel(at, 60.0, Modifiers::COMMAND);
    // The frame right after the gesture is the cheap interactive tier of a new window...
    let during = harness.wanted();
    assert!(during.window.is_some(), "zoomed must render a window");
    assert_eq!(during.quality, QualityTier::Interactive);
    assert_ne!(during.key, fit.key, "a new window is a new frame");
    // ...and once the wheel stops, the same zoom lands at full quality (the smoothed wheel
    // keeps delivering deltas for a few frames, so only the size class is pinned here).
    let settled = harness.settle(20);
    assert!(
        settled.window.is_some(),
        "the settled frame keeps the window"
    );
    assert_eq!(settled.quality, QualityTier::Settled);
    assert_eq!(settled.viewport_px, settled.display_px);
    assert!(
        settled.window.expect("window").1[0] < 1.0 && settled.window.expect("window").1[1] < 1.0,
        "{:?} is still the whole crop",
        settled.window
    );
    assert!(
        settled.display_px.0 >= fit.display_px.0,
        "zoomed fills the panel: {:?} vs {:?}",
        settled.display_px,
        fit.display_px
    );
}

#[test]
fn zooming_with_the_pointer_off_the_photo_does_nothing() {
    // The stage must not grab a wheel over the instrument panel.
    let mut harness = Harness::new();
    let fit = harness.wanted();
    let corner = Pos2::new(60.0, SCREEN.1 - 60.0);
    harness.wheel(corner, 60.0, Modifiers::COMMAND);
    assert_eq!(harness.settle(4), fit);
}

#[test]
fn a_drag_pans_the_window_without_resizing_it() {
    let mut harness = Harness::new();
    let zoomed = harness.zoom_in(2.0);
    assert!(zoomed.window.is_some());
    let at = harness.hover_photo();
    harness.press(at, true);
    for step in 1..=4 {
        harness.frame(vec![Event::PointerMoved(Pos2::new(
            at.x + 40.0 * step as f32,
            at.y,
        ))]);
    }
    harness.press(Pos2::new(at.x + 160.0, at.y), false);
    let panned = harness.settle(8);
    assert_eq!(
        panned.display_px, zoomed.display_px,
        "panning shows the same-sized window, moved"
    );
    assert_eq!(panned.viewport_px, zoomed.viewport_px);
    assert_ne!(panned.key, zoomed.key, "a moved window is a new frame");
    assert!(
        panned.origin()[0] < zoomed.origin()[0],
        "dragging right must move the image right: {:?} -> {:?}",
        zoomed.origin(),
        panned.origin()
    );
}

#[test]
fn a_trackpad_scroll_pans_the_zoomed_window() {
    let mut harness = Harness::new();
    let zoomed = harness.zoom_in(2.0);
    let at = harness.hover_photo();
    harness.wheel(at, 80.0, Modifiers::NONE);
    let panned = harness.settle(8);
    assert_eq!(panned.display_px, zoomed.display_px);
    assert_ne!(panned.key, zoomed.key);
    assert!(
        panned.origin()[1] < zoomed.origin()[1],
        "scrolling content down moves the image down: {:?} -> {:?}",
        zoomed.origin(),
        panned.origin()
    );
}

#[test]
fn plain_scroll_at_fit_pans_nothing() {
    // At fit there is nothing outside the panel to reveal: the wheel is left alone.
    let mut harness = Harness::new();
    let fit = harness.wanted();
    let at = harness.hover_photo();
    harness.wheel(at, 120.0, Modifiers::NONE);
    assert_eq!(harness.settle(4), fit);
}

#[test]
fn double_click_toggles_one_to_one_and_bare_z_returns_to_fit() {
    let mut harness = Harness::new();
    let fit = harness.wanted();
    let at = harness.hover_photo();
    // Two clicks on the stage within the double-click timeout.
    harness.press(at, true);
    harness.press(at, false);
    harness.frame(vec![Event::PointerMoved(at)]);
    harness.press(at, true);
    harness.press(at, false);
    let one = harness.settle(8);
    assert!(one.window.is_some(), "double-click zooms to 1:1");
    assert_ne!(one.key, fit.key);
    // Bare `Z` goes back to fit.
    harness.key(Key::Z, Modifiers::NONE);
    let back = harness.settle(8);
    assert_eq!(back.window, None);
    assert_eq!(back, fit);
}

#[test]
fn bare_z_under_an_open_modal_leaves_the_stage_at_fit() {
    // The stage's `Z` toggle is a global key handler; a modal owns the keyboard, so `Z` must not
    // zoom the photo hiding behind the dialog (same rule as culling keys under a confirmation).
    let mut harness = Harness::new();
    let fit = harness.wanted();
    harness.import = open_dialog();
    harness.settle(2);
    harness.key(Key::Z, Modifiers::NONE);
    let after = harness.settle(6);
    assert_eq!(after.window, None, "Z under a modal must not zoom");
    assert_eq!(after.key, fit.key);
}

#[test]
fn double_click_on_another_photo_resets_to_fit() {
    // A zoomed stage must not carry its window into the next photo.
    let mut harness = Harness::new();
    let zoomed = harness.zoom_in(2.0);
    harness.snapshot.editor.selected = Some(ImageId(8));
    harness.snapshot.editor.image = Some(ImageRow {
        id: 8,
        filename: "b.cr3".into(),
        width: Some(FIXTURE.0),
        height: Some(FIXTURE.1),
        ..Default::default()
    });
    let next = harness.settle(4);
    assert_eq!(next.window, None, "a new photo opens at fit");
    assert_ne!(next.key, zoomed.key);
}

#[test]
fn the_stage_gesture_target_never_eats_the_retry_button() {
    // The stage registers a full-panel click/drag target; a real widget painted later (the
    // worker-failure banner's Retry) must still receive its click. The scan stays inside the
    // stage's top margin, so a stray click can never hit a side panel and move the layout.
    let mut harness = Harness::new();
    harness.photo.failure = Some("develop failed".into());
    let image = harness
        .ui
        .develop
        .stage_image_rect()
        .expect("the stage drew the photo");
    let mut hit = false;
    'attempts: for row in 0..6 {
        for column in 0..20 {
            let at = Pos2::new(
                image.min.x + 20.0 + 20.0 * column as f32,
                image.min.y - 35.0 + 6.0 * row as f32,
            );
            harness.photo.retry = false;
            harness.frame(vec![Event::PointerMoved(at)]);
            harness.press(at, true);
            harness.press(at, false);
            if harness.photo.retry {
                hit = true;
                break 'attempts;
            }
        }
    }
    assert!(hit, "a click on the banner's Retry button must reach it");
}

#[test]
fn cmd_z_undoes_and_never_zooms() {
    // The stage's bare-`Z` toggle must not steal the undo shortcut.
    let mut harness = Harness::new();
    let fit = harness.wanted();
    let commands = harness.key(Key::Z, Modifiers::COMMAND);
    assert!(
        matches!(
            commands.first(),
            Some(Command::Edit(core_app::workflow::EditIntent::Undo))
        ),
        "Cmd+Z is still undo: {commands:?}"
    );
    assert_eq!(harness.settle(4).window, None);
    assert_eq!(harness.wanted(), fit);
}

impl Harness {
    /// One mouse-wheel (or trackpad) event over `at`.
    fn notch(&mut self, at: Pos2, dy: f32, source: MouseWheelSource, modifiers: Modifiers) {
        let unit = if source == MouseWheelSource::Wheel {
            MouseWheelUnit::Line
        } else {
            MouseWheelUnit::Point
        };
        let mut events = vec![Event::PointerMoved(at)];
        if modifiers != Modifiers::NONE {
            events.push(Event::ModifiersChanged(modifiers));
        }
        events.push(Event::MouseWheel {
            unit,
            delta: Vec2::new(0.0, dy),
            phase: TouchPhase::Move,
            source,
            modifiers,
        });
        self.frame(events);
    }
}

#[test]
fn a_mouse_wheel_zooms_in_anchored_at_the_pointer() {
    // Start already zoomed so the drawn rect fills the panel before and after: the fit->zoom
    // letterbox change would otherwise move the anchor by the rect-size difference.
    let mut harness = Harness::new();
    harness.zoom_in(2.0);
    let centre = harness.hover_photo();
    let rect = harness.ui.develop.stage_image_rect().expect("drawn");
    let before = harness.wanted().window.expect("zoomed");
    let at = centre + Vec2::new(rect.width() * 0.25, -rect.height() * 0.2);
    harness.frame(vec![Event::PointerMoved(at)]);
    harness.frame(vec![Event::PointerMoved(at)]);
    let uv_before = [
        before.0[0] + (at.x - rect.min.x) / rect.width() * before.1[0],
        before.0[1] + (at.y - rect.min.y) / rect.height() * before.1[1],
    ];
    harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    let zoomed = harness.settle(8);
    let (origin, size) = zoomed.window.expect("one notch zooms in");
    let rect = harness.ui.develop.stage_image_rect().expect("drawn");
    let uv_after = [
        origin[0] + (at.x - rect.min.x) / rect.width() * size[0],
        origin[1] + (at.y - rect.min.y) / rect.height() * size[1],
    ];
    for axis in 0..2 {
        assert!(
            (uv_after[axis] - uv_before[axis]).abs() < 1.5e-3,
            "{uv_before:?} -> {uv_after:?}"
        );
    }
    assert!(
        (size[0] / before.1[0] - 0.8).abs() < 0.01,
        "1.25x per notch: {size:?} vs {:?}",
        before.1
    );
}

#[test]
fn wheel_down_zooms_out_and_at_fit_does_nothing() {
    let mut harness = Harness::new();
    let fit = harness.wanted();
    let at = harness.hover_photo();
    harness.notch(at, -1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    assert_eq!(harness.settle(6), fit, "wheel-down at fit is a no-op");
    harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    assert!(harness.settle(6).window.is_some());
    harness.notch(at, -1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    // The wheel eases: the zoom lands a few frames in, and the settled tier follows.
    assert_eq!(
        harness.settle(12),
        fit,
        "one notch out of one notch in is fit"
    );
}

#[test]
fn a_wheel_zoom_does_not_also_pan() {
    let mut harness = Harness::new();
    let at = harness.hover_photo();
    harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    let first = harness.settle(10);
    let again = harness.settle(10);
    assert_eq!(first, again, "no residual pan after the notch");
}

#[test]
fn a_trackpad_scroll_pans_and_never_zooms() {
    let mut harness = Harness::new();
    let zoomed = harness.zoom_in(2.0);
    let at = harness.hover_photo();
    harness.notch(at, 80.0, MouseWheelSource::Trackpad, Modifiers::NONE);
    let panned = harness.settle(6);
    assert_eq!(panned.display_px, zoomed.display_px);
    assert_eq!(panned.window.expect("w").1, zoomed.window.expect("w").1);
    assert_ne!(panned.key, zoomed.key, "the window moved");
}

#[test]
fn cmd_trackpad_scroll_zooms() {
    let mut harness = Harness::new();
    let at = harness.hover_photo();
    harness.notch(at, 40.0, MouseWheelSource::Trackpad, Modifiers::COMMAND);
    assert!(harness.settle(6).window.is_some());
}

/// The crop-local point under `at` for the stage's last drawn rect and `window`.
fn uv_under(rect: Rect, at: Pos2, window: ([f32; 2], [f32; 2])) -> [f64; 2] {
    let (origin, size) = window;
    [
        f64::from(origin[0]) + f64::from((at.x - rect.min.x) / rect.width()) * f64::from(size[0]),
        f64::from(origin[1]) + f64::from((at.y - rect.min.y) / rect.height()) * f64::from(size[1]),
    ]
}

/// A zoomed stage with a wheel pointer off-centre, at 60 Hz, ready for notches.
fn eased_stage() -> (Harness, Pos2, f64) {
    let mut harness = Harness::new();
    harness.zoom_in(2.0);
    let centre = harness.hover_photo();
    let rect = harness.ui.develop.stage_image_rect().expect("drawn");
    let at = centre + Vec2::new(rect.width() * 0.25, -rect.height() * 0.2);
    harness.frame(vec![Event::PointerMoved(at)]);
    harness.frame(vec![Event::PointerMoved(at)]);
    harness.dt = 0.016;
    let size = f64::from(harness.wanted().window.expect("zoomed").1[0]);
    (harness, at, size)
}

#[test]
fn a_wheel_notch_eases_to_its_target_keeping_the_pointer_point_fixed() {
    let (mut harness, at, size0) = eased_stage();
    let rect = harness.ui.develop.stage_image_rect().expect("drawn");
    let window = harness.wanted().window.expect("zoomed");
    let anchor = uv_under(rect, at, window);
    harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    let mut sizes = Vec::new();
    for _ in 0..10 {
        let wanted = harness.wanted();
        let rect = harness.ui.develop.stage_image_rect().expect("drawn");
        let now = uv_under(rect, at, wanted.window.expect("zoomed"));
        for axis in 0..2 {
            // Half a physical pixel, in crop-local UV of the drawn window.
            let tolerance =
                0.5 / f64::from(rect.width()) * f64::from(wanted.window.unwrap().1[axis]);
            assert!(
                (now[axis] - anchor[axis]).abs() <= tolerance.max(1e-4),
                "frame drifted: {anchor:?} -> {now:?}"
            );
        }
        sizes.push(f64::from(wanted.window.expect("zoomed").1[0]));
        harness.frame(vec![Event::PointerMoved(at)]);
    }
    assert!(
        sizes.windows(2).all(|w| w[1] <= w[0] + 1e-6) && sizes[9] < sizes[0],
        "a smooth monotone zoom-in, not a jump: {sizes:?}"
    );
    assert!(
        sizes[0] > 0.9 * size0,
        "the first frame is only a step: {sizes:?}"
    );
    harness.settle(20);
    let landed = f64::from(harness.wanted().window.expect("zoomed").1[0]);
    assert!(
        (landed / (size0 * 0.8) - 1.0).abs() < 1e-3,
        "converged to 1.25x: {landed} vs {}",
        size0 * 0.8
    );
}

#[test]
fn a_new_notch_mid_animation_retargets_from_the_target() {
    let (mut harness, at, size0) = eased_stage();
    harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    harness.frame(vec![Event::PointerMoved(at)]);
    harness.frame(vec![Event::PointerMoved(at)]);
    let mid = f64::from(harness.wanted().window.expect("zoomed").1[0]);
    assert!(mid > size0 * 0.8, "still on the way to the first target");
    harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    harness.settle(30);
    let landed = f64::from(harness.wanted().window.expect("zoomed").1[0]);
    assert!(
        (landed / (size0 * 0.64) - 1.0).abs() < 1e-3,
        "two notches compound on the target (1.25^2), not on the half-way view: {landed}"
    );
    assert_eq!(harness.wanted().quality, QualityTier::Settled);
}

#[test]
fn the_wheel_never_zooms_past_four_hundred_percent() {
    let mut harness = Harness::new();
    let at = harness.hover_photo();
    for _ in 0..40 {
        harness.notch(at, 1.0, MouseWheelSource::Wheel, Modifiers::NONE);
    }
    let wanted = harness.settle(40);
    // Whatever the panel, 400 % is four screen pixels per source pixel.
    let (_, size) = wanted.window.expect("zoomed");
    let shown_px = f64::from(size[0]) * 6960.0;
    let displayed = f64::from(wanted.display_px.0);
    assert!(
        (displayed / shown_px - 4.0).abs() < 0.05,
        "400 % is four screen pixels per source pixel: {displayed} / {shown_px}"
    );
    assert!(wanted.viewport_px.0 <= wanted.display_px.0);
    assert!(
        wanted.viewport_px.0 < wanted.display_px.0,
        "above 100 % the render is native, i.e. smaller than what is shown"
    );
}
