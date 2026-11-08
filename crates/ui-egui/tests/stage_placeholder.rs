//! The stage compositor through the real panel: a cached placeholder is painted only when it is
//! compatible with what the stage wants, alone until a live frame exists, and under the fading
//! live frame when it arrives. Painted textures are read from the frame's mesh shapes.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::Command;
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use egui::{epaint::Shape, RawInput, Rect, TextureId};
use std::sync::Arc;
use ui_egui::{
    Mode, PhotoUi, Picture, PresentId, StageVariant, ThumbSource, ThumbState, UiState,
    SAVED_GEOMETRY,
};

const PLACEHOLDER: TextureId = TextureId::Managed(21);
const LIVE: TextureId = TextureId::Managed(22);

fn snapshot(edited_at: Option<i64>) -> ApplicationSnapshot {
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
                width: Some(6960),
                height: Some(4640),
                edited_at,
                ..Default::default()
            }),
            document: DocumentView {
                status: DocumentStatus::Ready,
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

struct Nothing;
impl ThumbSource for Nothing {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

fn picture(texture: TextureId, edit_version: Option<i64>) -> Picture {
    Picture {
        texture,
        px: [820, 547],
        id: PresentId {
            image: ImageId(7),
            edit_version,
            variant: StageVariant::Edited,
            geometry: SAVED_GEOMETRY,
        },
        window: None,
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
            ui: UiState {
                mode: Mode::Develop,
                ..Default::default()
            },
            photo: PhotoUi {
                loading: true,
                ..Default::default()
            },
            snapshot,
            time: 0.0,
        }
    }

    /// Run one frame; returns the textures painted as images and all painted text.
    fn frame(&mut self) -> (Vec<TextureId>, String) {
        self.time += 0.016;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1800.0, 1100.0),
            )),
            time: Some(self.time),
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let (mut error, mut notices) = (None, Vec::new());
        let mut output = self.ctx.run_ui(input, |root| {
            ui_egui::root(
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
        let (mut textures, mut text) = (Vec::new(), String::new());
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut textures, &mut text);
        }
        (textures, text)
    }
}

fn collect(shape: &Shape, textures: &mut Vec<TextureId>, text: &mut String) {
    match shape {
        Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, textures, text)),
        Shape::Mesh(mesh) if mesh.texture_id != TextureId::default() => {
            textures.push(mesh.texture_id)
        }
        Shape::Text(t) => text.push_str(t.galley.text()),
        _ => {}
    }
}

#[test]
fn a_compatible_placeholder_is_painted_alone_instead_of_the_spinner() {
    let mut h = Harness::new(snapshot(Some(3)));
    h.photo.placeholder = Some(picture(PLACEHOLDER, Some(3)));
    let (textures, text) = h.frame();
    assert!(textures.contains(&PLACEHOLDER));
    assert!(!text.contains("Loading photo"), "{text}");
}

#[test]
fn an_incompatible_placeholder_is_never_painted() {
    // The row is at edit version 3; the placeholder shows version 2.
    let mut h = Harness::new(snapshot(Some(3)));
    h.photo.placeholder = Some(picture(PLACEHOLDER, Some(2)));
    let (textures, text) = h.frame();
    assert!(!textures.contains(&PLACEHOLDER));
    assert!(
        text.contains("Loading photo"),
        "nothing paintable keeps the spinner"
    );
}

#[test]
fn the_live_frame_fades_in_over_the_placeholder_then_stands_alone() {
    let mut h = Harness::new(snapshot(None));
    h.photo.placeholder = Some(picture(PLACEHOLDER, None));
    h.frame();
    h.photo.texture = Some(picture(LIVE, None));
    let (textures, _) = h.frame();
    assert!(
        textures.contains(&PLACEHOLDER) && textures.contains(&LIVE),
        "mid-fade both are painted: {textures:?}"
    );
    h.time += 0.5;
    let (textures, _) = h.frame();
    assert_eq!(
        textures,
        vec![LIVE],
        "after the fade the live frame covers it"
    );
}
