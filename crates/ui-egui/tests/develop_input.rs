//! Headless egui input: synthetic pointer/keyboard events drive `ui_egui::root` over a Ready
//! snapshot and the emitted `Command`s are asserted. No window, no GPU.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, MirrorStatus, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::{Command, EditIntent};
use core_app::AppError;
use core_model::catalog::ImageRow;
use core_model::identity::{CatalogRevision, EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, UiState};

fn snapshot(can_undo: bool, can_redo: bool) -> ApplicationSnapshot {
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
                status: DocumentStatus::Ready,
                params: Some(Arc::new(DevelopParams::default())),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                dirty: false,
                gesture_active: false,
                can_undo,
                can_redo,
            },
            save: SaveStatus::default(),
            ..Default::default()
        },
        jobs: JobsSnapshot::default(),
        settings: SettingsSnapshot::default(),
        error: None,
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
    fn new(snapshot: ApplicationSnapshot) -> Self {
        let mut harness = Self {
            ctx: egui::Context::default(),
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
                placeholder: None,
            },
            snapshot,
            time: 0.0,
        };
        for _ in 0..3 {
            harness.frame(Vec::new(), Modifiers::NONE);
        }
        harness
    }

    fn frame(&mut self, events: Vec<Event>, modifiers: Modifiers) -> Vec<Command> {
        self.time += 0.05;
        let mut events = events;
        if modifiers != Modifiers::NONE {
            events.insert(0, Event::ModifiersChanged(modifiers));
        }
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 700.0))),
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
                    thumbs: &mut ui_egui::NoThumbs,
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

    fn slider(&self) -> Rect {
        self.ui.develop.exposure_rect.expect("slider laid out")
    }
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn button(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

fn point_at(rect: Rect, fraction: f32) -> Pos2 {
    Pos2::new(rect.left() + rect.width() * fraction, rect.center().y)
}

fn edits(commands: &[Command]) -> Vec<&EditIntent> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Edit(intent) => Some(intent),
            _ => None,
        })
        .collect()
}

/// Press at 30 %, drag to 60 %, return every command seen during the drag (pointer still down).
fn drag(h: &mut Harness) -> Vec<Command> {
    let rect = h.slider();
    let start = point_at(rect, 0.3);
    let mut seen = h.frame(vec![Event::PointerMoved(start)], Modifiers::NONE);
    seen.extend(h.frame(vec![button(start, true)], Modifiers::NONE));
    for fraction in [0.4, 0.5, 0.6] {
        let to = point_at(rect, fraction);
        seen.extend(h.frame(vec![Event::PointerMoved(to)], Modifiers::NONE));
    }
    seen
}

#[test]
fn drag_emits_begin_updates_and_commit() {
    let mut h = Harness::new(snapshot(false, false));
    let mut seen = drag(&mut h);
    let end = point_at(h.slider(), 0.6);
    seen.extend(h.frame(vec![button(end, false)], Modifiers::NONE));
    let intents = edits(&seen);
    assert_eq!(
        intents.first(),
        Some(&&EditIntent::BeginGesture),
        "{intents:?}"
    );
    assert_eq!(
        intents.last(),
        Some(&&EditIntent::CommitGesture),
        "{intents:?}"
    );
    let updates: Vec<f32> = intents
        .iter()
        .filter_map(|i| match i {
            EditIntent::UpdateGesture(p) => Some(p.exposure),
            _ => None,
        })
        .collect();
    assert!(!updates.is_empty(), "{intents:?}");
    assert!(updates.windows(2).all(|w| w[0] <= w[1]), "{updates:?}");
    assert!(*updates.last().unwrap() > 0.0, "{updates:?}");
    assert!(
        !intents
            .iter()
            .any(|i| matches!(i, EditIntent::SetExposure(_))),
        "a drag is a gesture, never discrete: {intents:?}"
    );
}

#[test]
fn escape_during_drag_cancels_and_release_commits_nothing() {
    let mut h = Harness::new(snapshot(false, false));
    let mut seen = drag(&mut h);
    seen.extend(h.frame(vec![key(Key::Escape, Modifiers::NONE)], Modifiers::NONE));
    let to = point_at(h.slider(), 0.8);
    seen.extend(h.frame(vec![Event::PointerMoved(to)], Modifiers::NONE));
    seen.extend(h.frame(vec![button(to, false)], Modifiers::NONE));
    let intents = edits(&seen);
    assert_eq!(
        intents.first(),
        Some(&&EditIntent::BeginGesture),
        "{intents:?}"
    );
    assert!(intents.contains(&&EditIntent::CancelGesture), "{intents:?}");
    assert!(
        !intents.contains(&&EditIntent::CommitGesture),
        "{intents:?}"
    );
    let cancel = intents
        .iter()
        .position(|i| **i == EditIntent::CancelGesture)
        .unwrap();
    assert!(
        intents[cancel + 1..].is_empty(),
        "nothing may follow the cancel: {intents:?}"
    );
}

#[test]
fn cmd_z_undoes_only_when_possible() {
    let z = |shift: bool| {
        let m = if shift {
            Modifiers::COMMAND | Modifiers::SHIFT
        } else {
            Modifiers::COMMAND
        };
        (vec![key(Key::Z, m)], m)
    };
    let mut h = Harness::new(snapshot(true, true));
    let (events, m) = z(false);
    assert_eq!(edits(&h.frame(events, m)), vec![&EditIntent::Undo]);
    let (events, m) = z(true);
    assert_eq!(edits(&h.frame(events, m)), vec![&EditIntent::Redo]);

    let mut h = Harness::new(snapshot(false, false));
    let (events, m) = z(false);
    assert!(edits(&h.frame(events, m)).is_empty());
    let (events, m) = z(true);
    assert!(edits(&h.frame(events, m)).is_empty());
}

#[test]
fn nothing_is_emitted_when_not_editable() {
    let mut snap = snapshot(true, true);
    snap.editor.document.status = DocumentStatus::Loading;
    snap.editor.document.can_undo = false;
    snap.editor.document.can_redo = false;
    let mut h = Harness::new(snap);
    let mut seen = drag(&mut h);
    let end = point_at(h.slider(), 0.6);
    seen.extend(h.frame(vec![button(end, false)], Modifiers::NONE));
    assert!(edits(&seen).is_empty(), "{seen:?}");
}

#[test]
fn save_failure_banner_offers_discard_and_dismiss() {
    let mut snap = snapshot(false, false);
    snap.error = Some(AppError::SaveFailed {
        image: "image 9".into(),
        message: "disk full".into(),
    });
    snap.editor.save = SaveStatus {
        dirty: true,
        mirror: MirrorStatus::Pending {
            owed_revision: CatalogRevision(3),
        },
        ..Default::default()
    };
    let mut h = Harness::new(snap);
    // Painted text is the observable contract: banner, Discard button naming the image, and
    // the two separate status lines.
    let mut output = h.ctx.run_ui(
        RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 700.0))),
            ..Default::default()
        },
        |root| {
            let mut error = None;
            let mut notices = Vec::new();
            ui_egui::root(
                root,
                ui_egui::Frame {
                    snapshot: &h.snapshot,
                    reopen: &Command::RefreshLibrary,
                    command_error: &mut error,
                    photo: &mut h.photo,
                    settings_notices: &mut notices,
                    thumbs: &mut ui_egui::NoThumbs,
                    histogram: None,
                    import: &core_app::import::ImportView::default(),
                    export: &core_app::export::ExportView::default(),
                },
                &mut h.ui,
            );
        },
    );
    output.textures_delta.clear();
    let text = format!("{:?}", output.shapes);
    assert!(
        text.contains("Discard unsaved changes to image 9"),
        "banner missing"
    );
    assert!(
        text.contains("Sidecar: pending (revision 3)"),
        "mirror line missing"
    );
    assert!(text.contains("Unsaved changes"), "catalog line missing");
}
