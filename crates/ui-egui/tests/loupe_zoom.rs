//! The Library loupe zooms like the Develop stage: wheel, double-click, Esc, reset on a new
//! photo — and a zoomed loupe asks the host for a viewport render, not a fitted frame.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, LibrarySnapshot,
};
use core_app::workflow::Command;
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion, QualityTier};
use core_model::DevelopParams;
use egui::{
    Event, Key, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, RawInput, Rect,
    TextureId, TouchPhase, Vec2,
};
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, ThumbSource, ThumbState, UiState};

fn row(id: i64) -> ImageRow {
    ImageRow {
        id,
        filename: format!("IMG_{id}.cr3"),
        width: Some(6960),
        height: Some(4640),
        ..Default::default()
    }
}

fn snapshot(selected: i64) -> ApplicationSnapshot {
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        library: LibrarySnapshot {
            count: 2,
            rows: Arc::new(vec![row(1), row(2)]),
            ..Default::default()
        },
        editor: EditorSnapshot {
            selected: Some(ImageId(selected)),
            image: Some(row(selected)),
            document: DocumentView {
                status: DocumentStatus::Ready,
                params: Some(Arc::new(DevelopParams::default())),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                ..Default::default()
            },
            ..Default::default()
        },
        catalog_epoch: Default::default(),
        jobs: Default::default(),
        settings: Default::default(),
        error: None,
    }
}

struct Nothing;
impl ThumbSource for Nothing {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

struct Harness {
    ctx: egui::Context,
    ui: UiState,
    photo: PhotoUi,
    snapshot: ApplicationSnapshot,
    time: f64,
}

impl Harness {
    fn new() -> Self {
        let mut h = Self {
            ctx: egui::Context::default(),
            ui: UiState {
                mode: Mode::Library,
                ..Default::default()
            },
            photo: PhotoUi {
                texture: Some((TextureId::Managed(7), [820, 547])),
                message: None,
                failure: None,
                retry: false,
                loading: false,
                device_epoch: Default::default(),
                plan: None,
            },
            snapshot: snapshot(1),
            time: 0.0,
        };
        h.ui.library.loupe = true;
        h.settle(4);
        h
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<Command> {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 800.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let (mut error, mut notices, mut commands) = (None, Vec::new(), Vec::new());
        let mut output = self.ctx.run_ui(input, |root| {
            commands = ui_egui::root(
                root,
                ui_egui::Frame {
                    snapshot: &self.snapshot,
                    reopen: &reopen,
                    command_error: &mut error,
                    photo: &mut self.photo,
                    settings_notices: &mut notices,
                    thumbs: &mut Nothing,
                    histogram: None,
                    import: &core_app::import::ImportView::default(),
                    export: &core_app::export::ExportView::default(),
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        commands
    }

    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(Vec::new());
        }
    }

    fn window(&self) -> Option<([f32; 2], [f32; 2])> {
        self.photo.plan.as_ref().expect("a plan").window
    }

    fn hover(&mut self) -> Pos2 {
        let at = Pos2::new(700.0, 400.0);
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![Event::PointerMoved(at)]);
        at
    }

    fn wheel(&mut self, at: Pos2, dy: f32) {
        self.frame(vec![
            Event::PointerMoved(at),
            Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: Vec2::new(0.0, dy),
                phase: TouchPhase::Move,
                source: MouseWheelSource::Wheel,
                modifiers: Modifiers::NONE,
            },
        ]);
    }

    fn click(&mut self, at: Pos2, pressed: bool) {
        self.frame(vec![Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }]);
    }

    fn key(&mut self, key: Key) {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }]);
    }
}

#[test]
fn the_wheel_zooms_the_loupe_into_a_full_quality_viewport_request() {
    let mut h = Harness::new();
    assert_eq!(h.window(), None, "the loupe opens at fit");
    let at = h.hover();
    h.wheel(at, 1.0);
    h.settle(8);
    let plan = h.photo.plan.as_ref().expect("plan");
    assert!(plan.window.is_some(), "zoomed loupe renders a window");
    assert_eq!(plan.key.quality, QualityTier::Settled);
    assert_eq!(plan.viewport_px, plan.display_px, "physical resolution");
}

#[test]
fn double_click_toggles_one_to_one_and_stays_in_the_loupe() {
    let mut h = Harness::new();
    let at = h.hover();
    for _ in 0..2 {
        h.click(at, true);
        h.click(at, false);
    }
    h.settle(8);
    assert!(h.window().is_some(), "double-click zooms to 1:1");
    assert!(h.ui.library.loupe);
    for _ in 0..2 {
        h.click(at, true);
        h.click(at, false);
    }
    h.settle(8);
    assert_eq!(h.window(), None);
}

#[test]
fn escape_leaves_the_loupe() {
    let mut h = Harness::new();
    h.key(Key::Escape);
    assert!(!h.ui.library.loupe);
}

#[test]
fn a_new_selection_resets_the_loupe_to_fit() {
    let mut h = Harness::new();
    let at = h.hover();
    h.wheel(at, 2.0);
    h.settle(8);
    assert!(h.window().is_some());
    h.snapshot = snapshot(2);
    h.settle(4);
    assert_eq!(h.window(), None);
}
