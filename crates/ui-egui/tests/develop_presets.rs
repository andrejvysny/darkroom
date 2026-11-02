//! Headless coverage of the Develop Presets and History tabs. Controls are driven through
//! AccessKit (the same names assistive technology sees) and the emitted `Command`s are asserted.
//! No window, no GPU.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::{Command, EditIntent, Mutation, SettingsSource};
use core_model::catalog::{ImageRow, PresetSummary, SnapshotSummary};
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use egui::accesskit::{Action, ActionData, ActionRequest, Node, NodeId, Role, TreeId};
use egui::{Event, Key, Modifiers, Pos2, RawInput, Rect};
use std::collections::HashMap;
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, ThumbSource, ThumbState, UiState};

fn preset(id: i64, name: &str, group: &str, builtin: bool) -> PresetSummary {
    PresetSummary {
        id,
        name: name.into(),
        group_name: group.into(),
        builtin,
        ..Default::default()
    }
}

fn snapshot(status: DocumentStatus) -> ApplicationSnapshot {
    let params = DevelopParams {
        exposure: 0.5,
        ..Default::default()
    };
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        catalog_epoch: Default::default(),
        library: LibrarySnapshot {
            presets: Arc::new(vec![
                preset(1, "Punchy", "Built-in", true),
                preset(2, "Soft Matte", "Built-in", true),
                preset(3, "Mine", "My Presets", false),
            ]),
            ..Default::default()
        },
        editor: EditorSnapshot {
            selected: Some(ImageId(7)),
            image: Some(ImageRow {
                id: 7,
                filename: "a.cr3".into(),
                ..Default::default()
            }),
            document: DocumentView {
                status,
                params: Some(Arc::new(params)),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                ..Default::default()
            },
            save: SaveStatus::default(),
            snapshots: Arc::new(vec![
                SnapshotSummary {
                    id: 11,
                    name: "Warm".into(),
                    created_at: 2,
                },
                SnapshotSummary {
                    id: 10,
                    name: "Cool".into(),
                    created_at: 1,
                },
            ]),
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
    tree: HashMap<NodeId, Node>,
    time: f64,
}

impl Harness {
    fn new(snapshot: ApplicationSnapshot) -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut harness = Self {
            ctx,
            ui: UiState {
                mode: Mode::Develop,
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
        };
        for _ in 0..4 {
            harness.frame(Vec::new());
        }
        harness
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<Command> {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 1400.0))),
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
                    import: &core_app::import::ImportView::default(),
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

    fn act(&mut self, target: NodeId, action: Action, data: Option<ActionData>) -> Vec<Command> {
        let seen = self.frame(vec![Event::AccessKitActionRequest(ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: target,
            data,
        })]);
        // The click flips UI state; the next frame renders it.
        let mut all = seen;
        all.extend(self.frame(Vec::new()));
        all
    }

    fn click(&mut self, label: &str) -> Vec<Command> {
        let (id, node) = self.node(Role::Button, label);
        assert!(!node.is_disabled(), "{label} is disabled");
        self.act(id, Action::Click, None)
    }

    fn open_history(&mut self) {
        let (id, _) = self.node(Role::Tab, "History");
        self.act(id, Action::Click, None);
    }

    fn type_text(&mut self, text: &str) {
        self.frame(vec![Event::Text(text.into())]);
    }

    fn press(&mut self, key: Key) -> Vec<Command> {
        let event = |pressed| Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let mut seen = self.frame(vec![event(true)]);
        seen.extend(self.frame(vec![event(false)]));
        seen.extend(self.frame(Vec::new()));
        seen
    }

    fn mentions(&self, text: &str) -> bool {
        self.tree
            .values()
            .any(|n| n.label() == Some(text) || n.value() == Some(text))
    }
}

fn apply_settings(commands: &[Command]) -> Vec<(&SettingsSource, f32, bool)> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::ApplySettings {
                source,
                amount,
                replace_all,
            } => Some((source, *amount, *replace_all)),
            _ => None,
        })
        .collect()
}

#[test]
fn clicking_a_preset_applies_it_at_full_amount_without_replacing() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready));
    let commands = h.click("Soft Matte");
    assert_eq!(
        apply_settings(&commands),
        [(&SettingsSource::Preset(2), 1.0, false)]
    );
    // User presets apply exactly like built-ins.
    let commands = h.click("Mine");
    assert_eq!(
        apply_settings(&commands),
        [(&SettingsSource::Preset(3), 1.0, false)]
    );
}

#[test]
fn amount_scales_presets_and_paste() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready));
    let (amount, node) = h.node(Role::Slider, "Amount");
    assert_eq!(node.numeric_value(), Some(100.0));
    h.act(
        amount,
        Action::SetValue,
        Some(ActionData::NumericValue(40.0)),
    );
    let commands = h.click("Punchy");
    assert_eq!(
        apply_settings(&commands),
        [(&SettingsSource::Preset(1), 0.4, false)]
    );
    h.click("Copy settings");
    let commands = h.click("Paste");
    let applied = apply_settings(&commands);
    assert_eq!(applied.len(), 1);
    assert_eq!(applied[0].1, 0.4);
}

#[test]
fn paste_waits_for_a_copy_then_applies_the_copied_document() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready));
    let (_, paste) = h.node(Role::Button, "Paste");
    assert!(paste.is_disabled());
    h.click("Copy settings");
    assert!(h.mentions("Settings copied"), "toast shown");
    let commands = h.click("Paste");
    let applied = apply_settings(&commands);
    assert_eq!(applied.len(), 1);
    let (SettingsSource::Copied { params, field_keys }, amount, replace_all) = applied[0] else {
        panic!("expected a copied source");
    };
    assert_eq!(params.exposure, 0.5);
    assert_eq!(*field_keys, None);
    assert_eq!((amount, replace_all), (1.0, false));
}

#[test]
fn the_copy_survives_a_photo_and_mode_switch() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready));
    h.click("Copy settings");
    h.ui.mode = Mode::Library;
    h.frame(Vec::new());
    h.ui.mode = Mode::Develop;
    let mut next = snapshot(DocumentStatus::Ready);
    next.editor.selected = Some(ImageId(8));
    h.snapshot = next;
    for _ in 0..3 {
        h.frame(Vec::new());
    }
    let (_, paste) = h.node(Role::Button, "Paste");
    assert!(!paste.is_disabled());
}

#[test]
fn nothing_applies_while_the_document_is_not_ready() {
    let mut h = Harness::new(snapshot(DocumentStatus::Loading));
    for label in ["Punchy", "Copy settings", "Paste"] {
        let (id, node) = h.node(Role::Button, label);
        assert!(node.is_disabled(), "{label}");
        assert!(h.act(id, Action::Click, None).is_empty());
    }
    h.open_history();
    for label in [
        "Revert to opened state",
        "Reset to default",
        "Create snapshot",
        "Warm",
        "Rename Warm",
        "Delete Warm",
    ] {
        let (id, node) = h.node(Role::Button, label);
        assert!(node.is_disabled(), "{label}");
        assert!(h.act(id, Action::Click, None).is_empty(), "{label}");
    }
}

#[test]
fn history_buttons_emit_their_edit_intents() {
    let mut snap = snapshot(DocumentStatus::Ready);
    snap.editor.document.can_undo = true;
    let mut h = Harness::new(snap);
    h.open_history();
    let (_, redo) = h.node(Role::Button, "Redo edit");
    assert!(redo.is_disabled(), "nothing to redo");
    let intent = |commands: Vec<Command>| match commands.as_slice() {
        [Command::Edit(intent)] => intent.clone(),
        other => panic!("expected one edit, got {other:?}"),
    };
    assert_eq!(intent(h.click("Undo edit")), EditIntent::Undo);
    assert_eq!(
        intent(h.click("Revert to opened state")),
        EditIntent::RevertToOpened
    );
    assert_eq!(
        intent(h.click("Reset to default")),
        EditIntent::ResetToDefault
    );
}

#[test]
fn creating_a_snapshot_names_it_on_enter_and_escape_cancels() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready));
    h.open_history();
    h.click("Create snapshot");
    h.type_text("Moody");
    let commands = h.press(Key::Enter);
    let creates: Vec<_> = commands
        .iter()
        .filter_map(|c| match c {
            Command::Mutate(Mutation::CreateSnapshot {
                image,
                name,
                params,
            }) => Some((*image, name.clone(), params.exposure)),
            _ => None,
        })
        .collect();
    assert_eq!(creates, [(ImageId(7), "Moody".to_owned(), 0.5)]);

    h.click("Create snapshot");
    h.type_text("Nope");
    assert!(h.press(Key::Escape).is_empty());
    // The field is gone: Enter now does nothing.
    assert!(h.press(Key::Enter).is_empty());
}

#[test]
fn a_snapshot_row_restores_whole_and_rename_and_delete_target_it() {
    let mut h = Harness::new(snapshot(DocumentStatus::Ready));
    h.open_history();
    let commands = h.click("Warm");
    assert_eq!(
        apply_settings(&commands),
        [(&SettingsSource::Snapshot(11), 1.0, true)]
    );
    let commands = h.click("Delete Cool");
    assert_eq!(commands, [Command::Mutate(Mutation::DeleteSnapshot(10))]);

    h.click("Rename Warm");
    // The field starts with the current name; replace it.
    h.frame(vec![Event::Key {
        key: Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    }]);
    h.type_text("Golden");
    let commands = h.press(Key::Enter);
    assert!(
        commands.contains(&Command::Mutate(Mutation::RenameSnapshot {
            id: 11,
            name: "Golden".into()
        })),
        "{commands:?}"
    );
}

#[test]
fn preset_management_stays_a_disabled_placeholder() {
    let h = Harness::new(snapshot(DocumentStatus::Ready));
    let placeholders: Vec<_> = h
        .tree
        .values()
        .filter(|n| n.role() == Role::Button && n.label() == Some(ui_egui::NOT_YET))
        .collect();
    assert!(placeholders.len() >= 2, "new-preset and import icons");
    assert!(placeholders.iter().all(|n| n.is_disabled()));
}
