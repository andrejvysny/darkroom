//! Before/After, the crop tool and the Border module, driven through the real Develop panel with
//! accessibility enabled (pills are clicked by AccessKit action, the stage by pointer events). The
//! snapshot is static, so only the commands the UI emits and the plan it asks the host for are
//! observable — which is exactly the contract.

use core_app::import::{ImportPhase, ImportView};
use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::{Command, EditIntent};
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::{Border, Crop, DevelopParams};
use egui::accesskit::{Action, ActionRequest, Node, NodeId, Role, Toggled, TreeId};
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, TextureId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use ui_egui::{Mode, PhotoRequestPlan, PhotoUi, ThumbSource, ThumbState, UiState};

const FIXTURE: (i64, i64) = (6000, 4000);

fn snapshot(params: DevelopParams) -> ApplicationSnapshot {
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
                params: Some(Arc::new(params)),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                ..Default::default()
            },
            save: SaveStatus::default(),
            ..Default::default()
        },
    }
}

struct NoThumbs;
impl ThumbSource for NoThumbs {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

struct Harness {
    ctx: egui::Context,
    ui: UiState,
    photo: PhotoUi,
    snapshot: ApplicationSnapshot,
    import: ImportView,
    tree: HashMap<NodeId, Node>,
    time: f64,
}

impl Harness {
    fn new(params: DevelopParams) -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut harness = Self {
            ctx,
            ui: UiState {
                mode: Mode::Develop,
                ..Default::default()
            },
            photo: PhotoUi {
                texture: Some(ui_egui::Picture::bare(TextureId::Managed(7), [820, 547])),
                message: None,
                failure: None,
                retry: false,
                loading: false,
                device_epoch: Default::default(),
                plan: None,
                placeholder: None,
            },
            snapshot: snapshot(params),
            import: ImportView::default(),
            tree: HashMap::new(),
            time: 0.0,
        };
        for _ in 0..4 {
            harness.frame(Vec::new());
        }
        harness
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<Command> {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 2600.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let mut error = None;
        let mut notices = Vec::new();
        let mut commands = Vec::new();
        let mut thumbs = NoThumbs;
        let mut output = self.ctx.run_ui(input, |root| {
            commands = ui_egui::root(
                root,
                ui_egui::Frame {
                    snapshot: &self.snapshot,
                    reopen: &reopen,
                    command_error: &mut error,
                    photo: &mut self.photo,
                    settings_notices: &mut notices,
                    thumbs: &mut thumbs,
                    histogram: None,
                    import: &self.import,
                    export: &core_app::export::ExportView::default(),
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        if let Some(update) = output.platform_output.accesskit_update {
            self.tree.extend(update.nodes);
        }
        commands
    }

    fn node(&self, role: Role, label: &str) -> (NodeId, &Node) {
        let found: Vec<_> = self
            .tree
            .iter()
            .filter(|(_, n)| n.role() == role && n.label() == Some(label))
            .collect();
        assert_eq!(found.len(), 1, "expected one {role:?} named {label:?}");
        (*found[0].0, found[0].1)
    }

    fn has(&self, label: &str) -> bool {
        self.tree
            .values()
            .any(|n| n.role() == Role::Button && n.label() == Some(label))
    }

    /// Click a button by name; returns the commands of the frame that handles the click.
    fn click(&mut self, label: &str) -> Vec<Command> {
        let (id, _) = self.node(Role::Button, label);
        let commands = self.frame(vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: id,
            data: None,
        })]);
        // The click flips UI state; a following frame renders it.
        self.frame(Vec::new());
        commands
    }

    fn open_module(&mut self, title: &str) {
        let (_, header) = self.node(Role::Button, title);
        if header.is_expanded() != Some(true) {
            self.click(title);
            self.frame(Vec::new());
        }
    }

    fn plan(&self) -> &PhotoRequestPlan {
        self.photo.plan.as_ref().expect("a Ready document plans")
    }

    fn toggled(&self, label: &str) -> Option<Toggled> {
        self.node(Role::Button, label).1.toggled()
    }

    fn key(&mut self, key: Key) -> Vec<Command> {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }])
    }

    fn image_rect(&mut self) -> Rect {
        self.frame(Vec::new());
        self.ui
            .develop
            .stage_image_rect()
            .expect("the stage drew the photo")
    }

    fn pointer(&mut self, at: Pos2) -> Vec<Command> {
        self.frame(vec![Event::PointerMoved(at)])
    }

    fn button(&mut self, at: Pos2, pressed: bool) -> Vec<Command> {
        self.frame(vec![Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }])
    }

    /// Hover a corner and press on it.
    fn grab(&mut self, at: Pos2) -> Vec<Command> {
        self.pointer(at);
        self.pointer(at);
        self.button(at, true)
    }
}

fn edited() -> DevelopParams {
    DevelopParams {
        exposure: 2.0,
        ..Default::default()
    }
}

fn intents(commands: &[Command]) -> Vec<&EditIntent> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Edit(intent) => Some(intent),
            _ => None,
        })
        .collect()
}

fn applies(commands: &[Command]) -> Vec<&DevelopParams> {
    intents(commands)
        .into_iter()
        .filter_map(|i| match i {
            EditIntent::Apply(p) => Some(&**p),
            _ => None,
        })
        .collect()
}

fn enter_crop(h: &mut Harness) {
    h.open_module("Crop & straighten");
    h.click("Adjust crop");
    assert!(h.plan().crop_preview, "crop mode plans the crop preview");
}

#[test]
fn backslash_toggles_the_before_picture() {
    let mut h = Harness::new(edited());
    let edited_key = h.plan().key;
    assert_eq!(h.plan().params.exposure, 2.0);
    h.key(Key::Backslash);
    h.frame(Vec::new());
    assert_eq!(*h.plan().params, DevelopParams::default());
    assert_ne!(h.plan().key, edited_key);
    h.key(Key::Backslash);
    h.frame(Vec::new());
    assert_eq!(
        h.plan().key,
        edited_key,
        "toggling back restores the edited key"
    );
}

#[test]
fn backslash_is_ignored_while_a_modal_owns_the_keyboard() {
    let mut h = Harness::new(edited());
    h.import = ImportView {
        open: true,
        source: Some(PathBuf::from("/src")),
        recursive: true,
        files: Arc::new(Vec::new()),
        phase: ImportPhase::Ready,
        generation: 1,
        ..Default::default()
    };
    h.frame(Vec::new());
    h.key(Key::Backslash);
    h.frame(Vec::new());
    assert_eq!(h.plan().params.exposure, 2.0);
}

#[test]
fn the_before_pill_reports_and_flips_its_selected_state() {
    let mut h = Harness::new(edited());
    assert_eq!(h.toggled("Before / After"), Some(Toggled::False));
    h.click("Before / After");
    assert_eq!(h.toggled("Before / After"), Some(Toggled::True));
    assert_eq!(*h.plan().params, DevelopParams::default());
    assert!(!h.plan().crop_preview);
}

#[test]
fn the_crop_pill_enters_crop_mode_and_reports_selected() {
    let mut h = Harness::new(edited());
    h.open_module("Crop & straighten");
    assert_eq!(h.toggled("Adjust crop"), Some(Toggled::False));
    let normal_key = h.plan().key;
    h.click("Adjust crop");
    assert_eq!(h.toggled("Done cropping"), Some(Toggled::True));
    assert!(h.plan().crop_preview);
    assert_ne!(h.plan().key, normal_key);
    assert_eq!(h.plan().params.exposure, 2.0, "only the crop is reset");
    h.click("Done cropping");
    assert!(!h.plan().crop_preview);
    assert_eq!(h.plan().key, normal_key);
}

#[test]
fn crop_mode_is_unavailable_while_before_is_shown_and_before_leaves_it() {
    let mut h = Harness::new(edited());
    enter_crop(&mut h);
    h.key(Key::Backslash);
    h.frame(Vec::new());
    assert!(!h.plan().crop_preview, "Before replaces the crop tool");
    assert_eq!(*h.plan().params, DevelopParams::default());
    let (_, pill) = h.node(Role::Button, "Adjust crop");
    assert!(pill.is_disabled());
    h.key(Key::Backslash);
    h.frame(Vec::new());
    assert!(!h.plan().crop_preview, "crop mode stays left");
}

#[test]
fn the_square_preset_is_one_apply_with_legacy_extents() {
    let mut h = Harness::new(DevelopParams::default());
    h.open_module("Crop & straighten");
    let commands = h.click("1:1");
    let applied = applies(&commands);
    assert_eq!(applied.len(), 1, "{commands:?}");
    let crop = applied[0].crop;
    assert!((crop.hw - 1.0 / 3.0).abs() < 1e-6, "{crop:?}");
    assert_eq!((crop.hh, crop.cx, crop.cy), (0.5, 0.5, 0.5));
    assert!(h.plan().crop_preview, "a preset enters crop mode");
}

#[test]
fn the_full_preset_restores_the_whole_frame() {
    let cropped = DevelopParams {
        crop: Crop {
            hw: 0.2,
            hh: 0.3,
            angle: 4.0,
            ..Crop::default()
        },
        ..Default::default()
    };
    let mut h = Harness::new(cropped);
    h.open_module("Crop & straighten");
    let commands = h.click("Full");
    let applied = applies(&commands);
    assert_eq!(applied.len(), 1);
    assert_eq!(
        applied[0].crop,
        Crop {
            angle: 4.0,
            ..Crop::default()
        }
    );
}

#[test]
fn crop_mode_refuses_zoom_and_keeps_the_fit_view() {
    let mut h = Harness::new(DevelopParams::default());
    enter_crop(&mut h);
    let at = h.image_rect().center();
    h.pointer(at);
    h.frame(vec![Event::PointerMoved(at), Event::Zoom(2.0)]);
    for _ in 0..6 {
        h.frame(Vec::new());
    }
    assert_eq!(h.plan().window, None);
    assert!(h.plan().crop_preview);
}

#[test]
fn a_corner_drag_streams_one_gesture_with_legacy_clamps() {
    let mut h = Harness::new(DevelopParams::default());
    enter_crop(&mut h);
    let rect = h.image_rect();
    let corner = rect.right_bottom();
    let mut seen = h.grab(corner);
    let (w, hgt) = (rect.width(), rect.height());
    seen.extend(h.pointer(corner + egui::vec2(-0.25 * w, -0.1 * hgt)));
    seen.extend(h.pointer(corner + egui::vec2(-0.5 * w, -0.5 * hgt)));
    seen.extend(h.button(corner + egui::vec2(-0.5 * w, -0.5 * hgt), false));
    let all = intents(&seen);
    assert!(
        matches!(all.first(), Some(EditIntent::BeginGesture)),
        "{all:?}"
    );
    assert!(
        matches!(all.last(), Some(EditIntent::CommitGesture)),
        "{all:?}"
    );
    let updates: Vec<&DevelopParams> = all
        .iter()
        .filter_map(|i| match i {
            EditIntent::UpdateGesture(p) => Some(&**p),
            _ => None,
        })
        .collect();
    assert!(updates.len() >= 2, "one update per changed move: {all:?}");
    let last = updates.last().expect("an update").crop;
    // SE corner by (-50%, -50%): right edge 0.5, bottom edge 0.5.
    assert!(
        (last.hw - 0.25).abs() < 1e-3 && (last.hh - 0.25).abs() < 1e-3,
        "{last:?}"
    );
    assert!(
        (last.cx - 0.25).abs() < 1e-3 && (last.cy - 0.25).abs() < 1e-3,
        "{last:?}"
    );
    assert_eq!(
        updates[0].exposure, 0.0,
        "other params ride along untouched"
    );
}

#[test]
fn a_drag_past_the_opposite_corner_stops_at_the_minimum_size() {
    let mut h = Harness::new(DevelopParams::default());
    enter_crop(&mut h);
    let rect = h.image_rect();
    let corner = rect.right_bottom();
    let mut seen = h.grab(corner);
    seen.extend(h.pointer(corner + egui::vec2(-3.0 * rect.width(), -3.0 * rect.height())));
    h.button(rect.left_top(), false);
    let last = intents(&seen)
        .into_iter()
        .rev()
        .find_map(|i| match i {
            EditIntent::UpdateGesture(p) => Some(p.crop),
            _ => None,
        })
        .expect("an update");
    // Both edges clamp to 0.02 * 2 from the fixed corner.
    assert!(
        (last.hw - 0.02).abs() < 1e-4 && (last.hh - 0.02).abs() < 1e-4,
        "{last:?}"
    );
}

#[test]
fn escape_mid_drag_cancels_the_gesture_and_keeps_crop_mode() {
    let mut h = Harness::new(DevelopParams::default());
    enter_crop(&mut h);
    let rect = h.image_rect();
    let corner = rect.right_bottom();
    let mut seen = h.grab(corner);
    seen.extend(h.pointer(corner + egui::vec2(-40.0, -30.0)));
    seen.extend(h.pointer(corner + egui::vec2(-60.0, -40.0)));
    seen.extend(h.key(Key::Escape));
    seen.extend(h.pointer(corner + egui::vec2(-90.0, -70.0)));
    seen.extend(h.button(corner, false));
    let all = intents(&seen);
    assert!(
        all.iter().any(|i| matches!(i, EditIntent::CancelGesture)),
        "{all:?}"
    );
    assert!(
        !all.iter().any(|i| matches!(i, EditIntent::CommitGesture)),
        "a cancelled drag never commits: {all:?}"
    );
    let updates_after_cancel = all
        .iter()
        .skip_while(|i| !matches!(i, EditIntent::CancelGesture))
        .filter(|i| matches!(i, EditIntent::UpdateGesture(_)))
        .count();
    assert_eq!(updates_after_cancel, 0, "{all:?}");
    h.frame(Vec::new());
    assert!(
        h.plan().crop_preview,
        "the first Esc only cancelled the drag"
    );
}

#[test]
fn escape_and_enter_leave_crop_mode() {
    for key in [Key::Escape, Key::Enter] {
        let mut h = Harness::new(DevelopParams::default());
        enter_crop(&mut h);
        h.key(key);
        h.frame(Vec::new());
        assert!(!h.plan().crop_preview, "{key:?}");
    }
}

#[test]
fn changing_the_photo_leaves_crop_mode() {
    let mut h = Harness::new(DevelopParams::default());
    enter_crop(&mut h);
    h.snapshot.editor.selected = Some(ImageId(8));
    h.snapshot.editor.image.as_mut().expect("image").id = 8;
    h.frame(Vec::new());
    h.frame(Vec::new());
    assert!(!h.plan().crop_preview);
}

#[test]
fn leaving_develop_leaves_crop_mode() {
    let mut h = Harness::new(DevelopParams::default());
    enter_crop(&mut h);
    h.ui.mode = Mode::Library;
    h.frame(Vec::new());
    h.ui.mode = Mode::Develop;
    h.frame(Vec::new());
    h.frame(Vec::new());
    assert!(!h.plan().crop_preview);
}

fn bordered(size: f32) -> DevelopParams {
    DevelopParams {
        border: Border {
            size,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn an_active_border_shrinks_the_rendered_photo() {
    let plain = Harness::new(DevelopParams::default());
    let framed = Harness::new(bordered(10.0));
    let (full, inset) = (plain.plan().display_px, framed.plan().display_px);
    assert!(
        inset.0 < full.0 && inset.1 < full.1,
        "{inset:?} vs {full:?}"
    );
    assert!(
        (f64::from(inset.0) / f64::from(inset.1) - 1.5).abs() < 0.01,
        "the photo keeps its aspect inside the margin"
    );
}

#[test]
fn the_border_preview_is_suppressed_for_before_crop_and_zoom() {
    let full = Harness::new(DevelopParams::default()).plan().display_px;
    let mut h = Harness::new(bordered(10.0));
    let framed = h.plan().display_px;
    assert_ne!(framed, full);
    h.key(Key::Backslash);
    h.frame(Vec::new());
    assert_eq!(h.plan().display_px, full, "Before");
    h.key(Key::Backslash);
    h.open_module("Crop & straighten");
    h.click("Adjust crop");
    assert_eq!(h.plan().display_px, full, "crop mode");
    h.click("Done cropping");
    assert_eq!(h.plan().display_px, framed);
    let at = h.image_rect().center();
    h.pointer(at);
    h.frame(vec![Event::PointerMoved(at), Event::Zoom(2.0)]);
    for _ in 0..6 {
        h.frame(Vec::new());
    }
    assert!(h.plan().window.is_some(), "zoomed");
    let reference = {
        let mut r = Harness::new(DevelopParams::default());
        let at = r.image_rect().center();
        r.pointer(at);
        r.frame(vec![Event::PointerMoved(at), Event::Zoom(2.0)]);
        for _ in 0..6 {
            r.frame(Vec::new());
        }
        r.plan().display_px
    };
    assert_eq!(h.plan().display_px, reference, "zoomed ignores the border");
}

#[test]
fn border_color_pills_apply_exact_colors_and_report_selection() {
    let mut black = bordered(5.0);
    black.border.color = [0.0, 0.0, 0.0];
    let mut h = Harness::new(black);
    h.open_module("Border");
    assert_eq!(h.toggled("Black"), Some(Toggled::True));
    assert_eq!(h.toggled("White"), Some(Toggled::False));
    let applied = h.click("White");
    let applied = applies(&applied);
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].border.color, [1.0, 1.0, 1.0]);

    let mut h = Harness::new(bordered(5.0));
    h.open_module("Border");
    let commands = h.click("Black");
    assert_eq!(applies(&commands)[0].border.color, [0.0, 0.0, 0.0]);
}

#[test]
fn border_aspect_pills_apply_the_legacy_ratios() {
    let mut h = Harness::new(DevelopParams::default());
    h.open_module("Border");
    assert_eq!(h.toggled("Original"), Some(Toggled::True));
    let commands = h.click("4:5");
    let applied = applies(&commands);
    assert_eq!(applied.len(), 1);
    assert_eq!(
        (applied[0].border.aspect_w, applied[0].border.aspect_h),
        (4.0, 5.0)
    );
    assert!(h.has("9:16") && h.has("Original"));
}
