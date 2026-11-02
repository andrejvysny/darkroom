//! Headless Export dialog: entry points, preview, Start capture and the Jpeg/PNG quality rule,
//! driven through `ui_egui::root` with a synthetic `ExportView`.

use core_app::export::{ExportFormat, ExportPhase, ExportRequest, ExportTarget, ExportView};
use core_app::state::{ApplicationSnapshot, EditorSnapshot, LibrarySnapshot};
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use std::path::PathBuf;
use std::sync::Arc;
use ui_egui::{NoThumbs, PhotoUi, UiState};

fn snapshot(selected: Option<i64>) -> ApplicationSnapshot {
    let rows: Vec<ImageRow> = (1..=3)
        .map(|id| ImageRow {
            id,
            filename: format!("IMG_{id:04}.cr3"),
            ..Default::default()
        })
        .collect();
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        library: LibrarySnapshot {
            count: 3,
            rows: Arc::new(rows),
            ..Default::default()
        },
        editor: EditorSnapshot {
            selected: selected.map(ImageId),
            ..Default::default()
        },
        catalog_epoch: Default::default(),
        jobs: Default::default(),
        settings: Default::default(),
        error: None,
    }
}

fn target(name: &str) -> ExportTarget {
    ExportTarget {
        image: ImageId(1),
        filename: name.to_owned(),
        date: "2026-06-06".to_owned(),
    }
}

fn view() -> ExportView {
    ExportView {
        open: true,
        targets: vec![target("A.CR3"), target("a.cr3")],
        dir: Some(PathBuf::from("/out/photos")),
        template: "{date}_{original}".to_owned(),
        ..Default::default()
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
        Self {
            ctx: egui::Context::default(),
            ui: UiState::default(),
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
            time: 0.0,
        }
    }

    fn frame(
        &mut self,
        view: &ExportView,
        modifiers: Modifiers,
        mut events: Vec<Event>,
    ) -> Vec<ExportRequest> {
        self.time += 0.05;
        events.insert(0, Event::ModifiersChanged(modifiers));
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 900.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = core_app::workflow::Command::RefreshLibrary;
        let (mut error, mut notices) = (None, Vec::new());
        let mut thumbs = NoThumbs;
        let mut output = self.ctx.run_ui(input, |root| {
            ui_egui::root(
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
                    export: view,
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        std::mem::take(&mut self.ui.export_requests)
    }

    fn settle(&mut self, view: &ExportView) {
        for _ in 0..3 {
            self.frame(view, Modifiers::NONE, Vec::new());
        }
    }

    fn rect(&self, name: &str) -> Option<egui::Rect> {
        self.ui
            .export
            .button_rects
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| *r)
    }

    fn click(&mut self, view: &ExportView, name: &str) -> Vec<ExportRequest> {
        let pos = self
            .rect(name)
            .unwrap_or_else(|| panic!("{name} laid out"))
            .center();
        let button = |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut seen = self.frame(view, Modifiers::NONE, vec![Event::PointerMoved(pos)]);
        seen.extend(self.frame(view, Modifiers::NONE, vec![button(true)]));
        seen.extend(self.frame(view, Modifiers::NONE, vec![button(false)]));
        seen
    }
}

fn cmd_shift() -> Modifiers {
    Modifiers::COMMAND.plus(Modifiers::SHIFT)
}

fn press_e() -> Event {
    Event::Key {
        key: Key::E,
        physical_key: Some(Key::E),
        pressed: true,
        repeat: false,
        modifiers: cmd_shift(),
    }
}

#[test]
fn cmd_shift_e_opens_for_the_selection_count() {
    let closed = ExportView::default();
    let mut h = Harness::new(snapshot(Some(1)));
    h.ui.library.selected.extend([1, 2]);
    h.frame(&closed, Modifiers::NONE, Vec::new());
    let requests = h.frame(&closed, cmd_shift(), vec![press_e()]);
    assert_eq!(
        requests,
        [ExportRequest::Open(vec![ImageId(1), ImageId(2)])]
    );
}

#[test]
fn cmd_shift_e_with_nothing_selected_does_nothing() {
    let closed = ExportView::default();
    let mut h = Harness::new(snapshot(None));
    h.frame(&closed, Modifiers::NONE, Vec::new());
    assert!(h.frame(&closed, cmd_shift(), vec![press_e()]).is_empty());
}

#[test]
fn preview_shows_the_first_resolved_name() {
    let mut h = Harness::new(snapshot(Some(1)));
    h.settle(&view());
    assert!(h.ui.export.dialog_shown);
    assert_eq!(h.ui.export.preview.as_deref(), Some("2026-06-06_A.jpg"));
    let png = ExportView {
        format: ExportFormat::Png,
        ..view()
    };
    h.settle(&png);
    assert_eq!(h.ui.export.preview.as_deref(), Some("2026-06-06_A.png"));
}

#[test]
fn export_button_emits_start_only_with_a_destination() {
    let mut h = Harness::new(snapshot(Some(1)));
    h.settle(&view());
    assert!(h
        .click(&view(), "export_start")
        .contains(&ExportRequest::Start));
    let no_dir = ExportView {
        dir: None,
        ..view()
    };
    h.settle(&no_dir);
    assert!(!h
        .click(&no_dir, "export_start")
        .contains(&ExportRequest::Start));
}

#[test]
fn choose_and_cancel_buttons_emit_their_requests() {
    let mut h = Harness::new(snapshot(Some(1)));
    h.settle(&view());
    assert!(h
        .click(&view(), "export_choose")
        .contains(&ExportRequest::PickDir));
    assert!(h
        .click(&view(), "export_close")
        .contains(&ExportRequest::Close));
}

#[test]
fn quality_slider_is_jpeg_only() {
    let mut h = Harness::new(snapshot(Some(1)));
    h.settle(&view());
    assert!(h.ui.export.quality_shown);
    h.settle(&ExportView {
        format: ExportFormat::Png,
        ..view()
    });
    assert!(!h.ui.export.quality_shown);
}

#[test]
fn a_running_export_offers_cancel_but_not_close() {
    let running = ExportView {
        phase: ExportPhase::Running {
            done: 1,
            failed: 0,
            total: 2,
        },
        ..view()
    };
    let mut h = Harness::new(snapshot(Some(1)));
    h.settle(&running);
    assert!(h.rect("export_close").is_none());
    assert!(h.rect("export_start").is_none());
    assert!(h
        .click(&running, "export_cancel")
        .contains(&ExportRequest::Cancel));
}

#[test]
fn user_presets_are_deletable() {
    let with_preset = ExportView {
        presets: vec![core_app::export::ExportPreset {
            name: "Web".into(),
            template: "web_{seq}".into(),
        }],
        ..view()
    };
    let mut h = Harness::new(snapshot(Some(1)));
    h.settle(&with_preset);
    assert!(h
        .click(&with_preset, "export_delete_preset")
        .contains(&ExportRequest::DeletePreset("Web".into())));
}
