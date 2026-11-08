//! Headless "Missing" marking: grid pill, accessible name and the info panel line.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, LibrarySnapshot,
};
use core_app::workflow::Command;
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use core_model::DevelopParams;
use egui::{Event, Pos2, RawInput, Rect};
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, Presence, ThumbSource, ThumbState, UiState};

fn snapshot(selected: Option<i64>) -> ApplicationSnapshot {
    let rows: Vec<ImageRow> = (1..=3)
        .map(|id| ImageRow {
            id,
            filename: format!("IMG_{id:04}.cr3"),
            path: format!("/vol/IMG_{id:04}.cr3"),
            ..Default::default()
        })
        .collect();
    let image = selected.and_then(|id| rows.iter().find(|r| r.id == id).cloned());
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
            image,
            document: DocumentView {
                status: DocumentStatus::Ready,
                params: Some(Arc::new(DevelopParams::default())),
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

/// Row 2 is missing, row 1 present, row 3 unknown (or all unknown when `report` is false).
struct Thumbs {
    report: bool,
}

impl ThumbSource for Thumbs {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
    fn presence(&mut self, image: &ImageRow) -> Presence {
        match (self.report, image.id) {
            (true, 1) => Presence::Present,
            (true, 2) => Presence::Missing,
            _ => Presence::Unknown,
        }
    }
}

fn collect_text(shape: &egui::Shape, out: &mut String) {
    match shape {
        egui::Shape::Text(t) => {
            out.push_str(t.galley.text());
            out.push('\n');
        }
        egui::Shape::Vec(v) => v.iter().for_each(|s| collect_text(s, out)),
        _ => {}
    }
}

/// Painted text and AccessKit labels after a few frames.
fn render(selected: Option<i64>, report: bool) -> (String, Vec<String>) {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let (mut ui_state, mut text, mut labels) = (
        UiState {
            mode: Mode::Library,
            ..Default::default()
        },
        String::new(),
        Vec::new(),
    );
    let mut photo = PhotoUi {
        texture: None,
        message: None,
        failure: None,
        retry: false,
        loading: false,
        device_epoch: Default::default(),
        plan: None,
        placeholder: None,
    };
    let snapshot = snapshot(selected);
    for step in 0..3 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 1600.0))),
            time: Some(0.05 * f64::from(step + 1)),
            events: Vec::<Event>::new(),
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let (mut error, mut notices) = (None, Vec::new());
        let mut output = ctx.run_ui(input, |root| {
            ui_egui::root(
                root,
                ui_egui::Frame {
                    snapshot: &snapshot,
                    reopen: &reopen,
                    command_error: &mut error,
                    photo: &mut photo,
                    settings_notices: &mut notices,
                    thumbs: &mut Thumbs { report },
                    histogram: None,
                    import: &core_app::import::ImportView::default(),
                    export: &core_app::export::ExportView::default(),
                },
                &mut ui_state,
            );
        });
        text.clear();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut text);
        }
        output.textures_delta.clear();
        if let Some(update) = output.platform_output.accesskit_update {
            labels.extend(
                update
                    .nodes
                    .iter()
                    .filter_map(|(_, node)| node.label().map(str::to_owned)),
            );
        }
    }
    (text, labels)
}

#[test]
fn missing_row_gets_a_pill_and_an_accessible_suffix() {
    let (text, labels) = render(None, true);
    assert_eq!(text.matches("Missing").count(), 1, "{text}");
    assert!(
        labels.contains(&"IMG_0002.cr3, missing".to_owned()),
        "{labels:?}"
    );
    assert!(labels.contains(&"IMG_0001.cr3".to_owned()), "{labels:?}");
    assert!(labels.contains(&"IMG_0003.cr3".to_owned()), "{labels:?}");
}

#[test]
fn unknown_and_present_rows_show_no_badge() {
    let (text, labels) = render(Some(1), false);
    assert!(!text.contains("Missing"), "{text}");
    assert!(
        labels.iter().all(|l| !l.ends_with(", missing")),
        "{labels:?}"
    );
    let (text, _) = render(Some(1), true);
    assert_eq!(
        text.matches("Missing").count(),
        1,
        "only the missing row: {text}"
    );
}

#[test]
fn info_panel_names_the_path_of_a_missing_selection() {
    let (text, _) = render(Some(2), true);
    assert!(text.contains("Missing — /vol/IMG_0002.cr3"), "{text}");
}
