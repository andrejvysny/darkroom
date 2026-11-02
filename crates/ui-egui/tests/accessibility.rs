//! Headless AccessKit coverage: with accessibility enabled, `ui_egui::root` must publish named
//! controls with roles, values and states, and assistive-technology actions must reach the
//! same commands as pointer/keyboard input. No window, no GPU.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::{Command, EditIntent};
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use egui::accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, TreeId};
use egui::{Event, Pos2, RawInput, Rect};
use std::collections::HashMap;
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, ThumbSource, ThumbState, UiState};

fn snapshot(status: DocumentStatus) -> ApplicationSnapshot {
    let image = ImageRow {
        id: 7,
        filename: "a.cr3".into(),
        ..Default::default()
    };
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        catalog_epoch: Default::default(),
        library: LibrarySnapshot::default(),
        editor: EditorSnapshot {
            selected: Some(ImageId(7)),
            image: Some(image),
            document: DocumentView {
                status,
                params: Some(Arc::new(DevelopParams::default())),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                dirty: false,
                gesture_active: false,
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
    /// The accessibility tree as an adapter would hold it: every update merged in.
    tree: HashMap<NodeId, Node>,
    time: f64,
    import: core_app::import::ImportView,
}

impl Harness {
    fn new(snapshot: ApplicationSnapshot, mode: Mode) -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut harness = Self {
            ctx,
            ui: UiState {
                mode,
                ..Default::default()
            },
            photo: PhotoUi {
                texture: None,
                message: None,
                failure: None,
                retry: false,
                loading: false,
                device_epoch: Default::default(),
                plan: None,
            },
            snapshot,
            tree: HashMap::new(),
            time: 0.0,
            import: Default::default(),
        };
        for _ in 0..4 {
            harness.frame(Vec::new());
        }
        harness
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<Command> {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 2600.0))),
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

    /// The unique node with `role` whose label is `label`.
    fn node(&self, role: Role, label: &str) -> (NodeId, &Node) {
        let found: Vec<_> = self
            .tree
            .iter()
            .filter(|(_, n)| n.role() == role && n.label() == Some(label))
            .collect();
        assert_eq!(found.len(), 1, "expected one {role:?} named {label:?}");
        (*found[0].0, found[0].1)
    }

    fn act(&mut self, target: NodeId, action: Action, data: Option<ActionData>) -> Vec<Command> {
        self.frame(vec![Event::AccessKitActionRequest(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: target,
            data,
        })])
    }
}

/// Applied exposure values in hundredths of a stop (the slider's two-decimal resolution).
fn applied_exposure(commands: &[Command]) -> Vec<i32> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Edit(EditIntent::Apply(params)) => {
                Some((params.exposure * 100.0).round() as i32)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn develop_slider_is_a_named_slider_with_value_bounds_and_actions() {
    let h = Harness::new(snapshot(DocumentStatus::Ready), Mode::Develop);
    let (_, exposure) = h.node(Role::Slider, "Exposure");
    assert_eq!(exposure.numeric_value(), Some(0.0));
    assert_eq!(exposure.min_numeric_value(), Some(-5.0));
    assert_eq!(exposure.max_numeric_value(), Some(5.0));
    assert_eq!(exposure.value(), Some("0.00"));
    assert!(!exposure.is_disabled());
    for action in [Action::SetValue, Action::Increment, Action::Decrement] {
        assert!(exposure.supports_action(action), "{action:?}");
    }
}

#[test]
fn increment_and_set_value_apply_one_discrete_edit() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready), Mode::Develop);
    let (id, _) = h.node(Role::Slider, "Exposure");
    assert_eq!(applied_exposure(&h.act(id, Action::Increment, None)), [5]);
    assert_eq!(applied_exposure(&h.act(id, Action::Decrement, None)), [-5]);
    let set = h.act(id, Action::SetValue, Some(ActionData::NumericValue(1.25)));
    assert_eq!(applied_exposure(&set), [125]);
    let clamped = h.act(id, Action::SetValue, Some(ActionData::NumericValue(99.0)));
    assert_eq!(applied_exposure(&clamped), [500]);
    let nan = h.act(
        id,
        Action::SetValue,
        Some(ActionData::NumericValue(f64::NAN)),
    );
    assert!(applied_exposure(&nan).is_empty());
}

#[test]
fn sliders_are_disabled_and_inert_when_the_document_is_not_ready() {
    let mut h = Harness::new(snapshot(DocumentStatus::Loading), Mode::Develop);
    let (id, exposure) = h.node(Role::Slider, "Exposure");
    assert!(exposure.is_disabled());
    assert!(!exposure.supports_action(Action::SetValue));
    let commands = h.act(id, Action::SetValue, Some(ActionData::NumericValue(1.0)));
    assert!(applied_exposure(&commands).is_empty());
}

fn library_snapshot(selected: i64) -> ApplicationSnapshot {
    let rows: Vec<ImageRow> = (1..=4)
        .map(|id| ImageRow {
            id,
            filename: format!("IMG_{id:04}.cr3"),
            stars: if id == 2 { 3 } else { 0 },
            flag: if id == 3 {
                "pick".into()
            } else {
                String::new()
            },
            ..Default::default()
        })
        .collect();
    let mut snap = snapshot(DocumentStatus::Ready);
    snap.editor.selected = Some(ImageId(selected));
    snap.editor.image = rows.iter().find(|r| r.id == selected).cloned();
    snap.library = LibrarySnapshot {
        count: 4,
        rows: Arc::new(rows),
        ..Default::default()
    };
    snap
}

#[test]
fn library_cells_are_named_options_with_selection_state() {
    let h = Harness::new(library_snapshot(1), Mode::Library);
    let (_, first) = h.node(Role::ListBoxOption, "IMG_0001.cr3");
    assert_eq!(first.is_selected(), Some(true));
    let (_, second) = h.node(Role::ListBoxOption, "IMG_0002.cr3, 3 stars");
    assert_eq!(second.is_selected(), Some(false));
    let (_, third) = h.node(Role::ListBoxOption, "IMG_0003.cr3, Pick");
    assert_eq!(third.is_selected(), Some(false));
}

#[test]
fn accesskit_click_on_a_cell_selects_like_a_pointer_click() {
    let mut h = Harness::new(library_snapshot(1), Mode::Library);
    let (id, _) = h.node(Role::ListBoxOption, "IMG_0004.cr3");
    let commands = h.act(id, Action::Click, None);
    assert!(
        commands.contains(&Command::SelectImage(Some(ImageId(4)))),
        "{commands:?}"
    );
}

#[test]
fn search_field_is_a_named_text_input() {
    let h = Harness::new(library_snapshot(1), Mode::Library);
    h.node(Role::TextInput, "Search library");
}

#[test]
fn icon_buttons_are_named_by_tooltip_and_report_disabled() {
    let h = Harness::new(snapshot(DocumentStatus::Ready), Mode::Develop);
    let (_, undo) = h.node(Role::Button, "Undo");
    assert!(undo.is_disabled(), "nothing to undo");
    let (_, redo) = h.node(Role::Button, "Redo");
    assert!(redo.is_disabled());
}

#[test]
fn hsl_swatches_are_radio_buttons_with_one_selected() {
    let h = Harness::new(snapshot(DocumentStatus::Ready), Mode::Develop);
    let (_, red) = h.node(Role::RadioButton, "Red band");
    assert_eq!(red.is_selected(), Some(true));
    for band in [
        "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
    ] {
        let (_, node) = h.node(Role::RadioButton, &format!("{band} band"));
        assert_eq!(node.is_selected(), Some(false), "{band}");
    }
}

#[test]
fn module_headers_are_buttons_with_expanded_state() {
    let h = Harness::new(snapshot(DocumentStatus::Ready), Mode::Develop);
    let (_, light) = h.node(Role::Button, "Light");
    assert_eq!(light.is_expanded(), Some(true));
    let (_, balance) = h.node(Role::Button, "Color balance");
    assert_eq!(balance.is_expanded(), Some(false));
    h.node(Role::Button, "Reset Light");
}

#[test]
fn import_dialog_is_a_named_dialog() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready), Mode::Library);
    h.import.open = true;
    h.frame(Vec::new());
    h.frame(Vec::new());
    h.node(Role::Dialog, "Import photos");
}
