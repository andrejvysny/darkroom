//! The render tier the Develop stage asks for, measured through the real panel: `ui_egui::root`
//! runs headlessly over a Ready snapshot and the resulting `PhotoUi::plan` is asserted. The unit
//! tests in `photo.rs`/`develop/stage.rs` pin the geometry helpers; this pins that the panel
//! actually feeds `gesture_active` into the plan, that the fit is in PHYSICAL pixels, and that a
//! half-size interactive frame can be presented without shrinking the stage.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::Command;
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion, QualityTier};
use core_model::DevelopParams;
use egui::{RawInput, Rect, TextureId};
use std::sync::Arc;
use ui_egui::{Mode, PhotoRequestPlan, PhotoUi, ThumbSource, ThumbState, UiState};

/// The 3:2 fixture: its frame is width-bound in the develop stage's wide panel.
const FIXTURE: (i64, i64) = (6960, 4640);
const SCREEN: f32 = 1800.0;

fn snapshot(gesture_active: bool) -> ApplicationSnapshot {
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        catalog_epoch: Default::default(),
        library: LibrarySnapshot::default(),
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
                dirty: false,
                gesture_active,
                can_undo: false,
                can_redo: false,
            },
            save: SaveStatus::default(),
            ..Default::default()
        },
        jobs: JobsSnapshot::default(),
        settings: SettingsSnapshot::default(),
        error: None,
    }
}

struct Nothing;
impl ThumbSource for Nothing {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

fn photo(texture: Option<(TextureId, [u32; 2])>) -> PhotoUi {
    PhotoUi {
        texture,
        message: None,
        failure: None,
        retry: false,
        loading: false,
        device_epoch: Default::default(),
        plan: None,
    }
}

struct Harness {
    ctx: egui::Context,
    ui: UiState,
    photo: PhotoUi,
    snapshot: ApplicationSnapshot,
    thumbs: Nothing,
    ppp: f32,
    time: f64,
}

impl Harness {
    /// Warm frames first: the develop side panels need a layout pass before the stage's rect
    /// settles, and the plan must not depend on the first frame's transient layout.
    fn new(
        snapshot: ApplicationSnapshot,
        ppp: f32,
        texture: Option<(TextureId, [u32; 2])>,
    ) -> Self {
        let mut harness = Self {
            ctx: egui::Context::default(),
            ui: UiState {
                mode: Mode::Develop,
                ..Default::default()
            },
            photo: photo(texture),
            snapshot,
            thumbs: Nothing,
            ppp,
            time: 0.0,
        };
        harness.ctx.set_pixels_per_point(ppp);
        for _ in 0..4 {
            harness.frame();
        }
        harness
    }

    fn frame(&mut self) {
        self.time += 0.05;
        self.ctx.set_pixels_per_point(self.ppp);
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(SCREEN, 1100.0),
            )),
            time: Some(self.time),
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
                    import: &core_app::import::ImportView::default(),
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        let _ = commands;
    }

    fn plan(&self) -> &PhotoRequestPlan {
        self.photo
            .plan
            .as_ref()
            .expect("a Ready document with params plans a frame")
    }
}

/// The frame is aspect-correct (never panel-shaped) and sized to the stage, not degenerate.
fn assert_plausible_frame(plan: &PhotoRequestPlan, ppp: f32) {
    let (width, height) = plan.display_px;
    assert!(
        width > 400 && height > 200,
        "{width}×{height} is no stage frame"
    );
    assert!(
        f64::from(width) <= f64::from(SCREEN * ppp) + 1.0
            && f64::from(height) <= f64::from(1100.0 * ppp) + 1.0,
        "{width}×{height} exceeds the window"
    );
    let aspect = f64::from(width) / f64::from(height);
    assert!(
        (aspect - 1.5).abs() <= 1.0 / f64::from(height.max(1)),
        "{width}×{height} is not the 3:2 frame"
    );
}

#[test]
fn a_settled_document_asks_for_the_full_fitted_frame() {
    let harness = Harness::new(snapshot(false), 1.0, None);
    let plan = harness.plan();
    assert_eq!(plan.key.quality, QualityTier::Settled);
    assert_eq!(plan.viewport_px, plan.display_px);
    assert_plausible_frame(plan, 1.0);
}

#[test]
fn a_drag_asks_for_half_size_at_the_interactive_tier_and_displays_the_same_area() {
    let settled = Harness::new(snapshot(false), 1.0, None);
    let drag = Harness::new(snapshot(true), 1.0, None);
    let (settled_plan, drag_plan) = (settled.plan(), drag.plan());
    assert_eq!(settled_plan.key.quality, QualityTier::Settled);
    assert_eq!(drag_plan.key.quality, QualityTier::Interactive);
    // Same displayed area, quarter the pixels asked for.
    assert_eq!(drag_plan.display_px, settled_plan.display_px);
    assert_eq!(
        drag_plan.viewport_px,
        (settled_plan.display_px.0 / 2, settled_plan.display_px.1 / 2)
    );
    // A different key, or `PhotoView::submit` would keep the settled frame and the drag would
    // never land a single updated picture.
    assert_ne!(drag_plan.key, settled_plan.key);
    let mut same_but_geometry = drag_plan.key.identity;
    same_but_geometry.viewport_revision = settled_plan.key.identity.viewport_revision;
    assert_eq!(
        same_but_geometry, settled_plan.key.identity,
        "only the render geometry/tier may differ"
    );
    assert_plausible_frame(drag_plan, 1.0);
}

#[test]
fn presenting_a_half_size_frame_survives_the_draw() {
    // The stage sizes the drawn rect from the panel and the frame's aspect, so a half-size
    // interactive frame fills the settled rect instead of shrinking the picture to a quarter.
    let drag = Harness::new(snapshot(true), 1.0, None);
    let render_px = drag.plan().viewport_px;
    let mut presented = Harness::new(
        snapshot(true),
        1.0,
        Some((TextureId::Managed(1), [render_px.0, render_px.1])),
    );
    presented.frame();
    assert_eq!(presented.plan().key, drag.plan().key);
}

#[test]
fn the_fitted_size_is_physical_pixels() {
    // Retina: the same window at ppp 2 asks for the same area in twice the pixels.
    let once = Harness::new(snapshot(false), 1.0, None);
    let twice = Harness::new(snapshot(false), 2.0, None);
    let (one, two) = (once.plan(), twice.plan());
    assert_plausible_frame(one, 1.0);
    assert_plausible_frame(two, 2.0);
    for axis in 0..2 {
        let single = if axis == 0 {
            one.display_px.0
        } else {
            one.display_px.1
        };
        let double = if axis == 0 {
            two.display_px.0
        } else {
            two.display_px.1
        };
        assert!(
            (f64::from(double) - 2.0 * f64::from(single)).abs() <= 2.0,
            "ppp 2 gave {double} where ppp 1 gave {single}"
        );
    }
    assert_eq!(one.key.scale_milli, 1000);
    assert_eq!(two.key.scale_milli, 2000);
}
