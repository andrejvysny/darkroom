//! The stage footer's zoom readout and preset menu, driven through the real Develop panel with
//! accessibility enabled: entries are ordinary named buttons, clicked by AccessKit action.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SettingsSnapshot,
};
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use egui::accesskit::{Action, ActionRequest, Node, NodeId, Role, TreeId};
use egui::{Event, Pos2, RawInput, Rect, TextureId};
use std::collections::HashMap;
use std::sync::Arc;
use ui_egui::{Mode, PhotoRequestPlan, PhotoUi, ThumbSource, ThumbState, UiState};

/// Landscape fixture big enough that every preset is a real zoom-in.
const FIXTURE: (i64, i64) = (6960, 4640);
const SCREEN: (f32, f32) = (1800.0, 1100.0);

struct NoThumbs;
impl ThumbSource for NoThumbs {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

fn snapshot(size: (i64, i64)) -> ApplicationSnapshot {
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
                width: Some(size.0),
                height: Some(size.1),
                ..Default::default()
            }),
            document: DocumentView {
                status: DocumentStatus::Ready,
                params: Some(Arc::new(DevelopParams::default())),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                ..Default::default()
            },
            ..Default::default()
        },
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
    fn new(size: (i64, i64)) -> Self {
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
                ..Default::default()
            },
            snapshot: snapshot(size),
            tree: HashMap::new(),
            time: 0.0,
        };
        for _ in 0..4 {
            harness.frame(Vec::new());
        }
        harness
    }

    fn frame(&mut self, events: Vec<Event>) {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(SCREEN.0, SCREEN.1),
            )),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = core_app::workflow::Command::RefreshLibrary;
        let (mut error, mut notices, mut thumbs) = (None, Vec::new(), NoThumbs);
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
                    export: &core_app::export::ExportView::default(),
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        if let Some(update) = output.platform_output.accesskit_update {
            // Every update is the full tree: a closed popup's buttons must not linger.
            self.tree = update.nodes.into_iter().collect();
        }
    }

    fn button(&self, label: &str) -> Option<NodeId> {
        let found: Vec<_> = self
            .tree
            .iter()
            .filter(|(_, n)| n.role() == Role::Button && n.label() == Some(label))
            .map(|(id, _)| *id)
            .collect();
        assert!(found.len() <= 1, "two buttons named {label:?}");
        found.first().copied()
    }

    fn click(&mut self, label: &str) {
        let id = self
            .button(label)
            .unwrap_or_else(|| panic!("no button named {label:?}"));
        self.frame(vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node: id,
            data: None,
        })]);
        for _ in 0..4 {
            self.frame(Vec::new());
        }
    }

    fn plan(&self) -> &PhotoRequestPlan {
        self.photo.plan.as_ref().expect("a Ready document plans")
    }

    /// The preset labels currently offered by the open menu (the readout itself excluded).
    fn menu(&self) -> Vec<&'static str> {
        ["Fit", "100%", "200%", "400%"]
            .into_iter()
            .filter(|label| self.button(label).is_some())
            .collect()
    }
}

#[test]
fn the_readout_says_fit_and_its_menu_lists_the_presets() {
    let mut h = Harness::new(FIXTURE);
    assert_eq!(h.menu(), ["Fit"], "closed: only the readout, reading Fit");
    h.click("Fit");
    // The readout's own "Fit" and the menu's "Fit" share a label; the click would have found two
    // buttons if both existed, so the open menu is exercised through its other entries.
    assert!(h.button("100%").is_some() && h.button("200%").is_some() && h.button("400%").is_some());
}

#[test]
fn choosing_a_preset_zooms_to_that_percentage_of_actual_pixels() {
    let mut h = Harness::new(FIXTURE);
    h.click("Fit");
    h.click("100%");
    let (_, size) = h.plan().window.expect("100 % is a zoom-in");
    // One source pixel per screen pixel: the window covers `panel px` source pixels.
    let shown = f64::from(size[0]) * FIXTURE.0 as f64;
    assert!(
        (shown - f64::from(h.plan().display_px.0)).abs() < 1.5,
        "{shown}"
    );
    assert!(!h.plan().nearest, "100 % is not magnified");
    assert!(h.button("100%").is_some(), "the readout reads 100%");

    h.click("100%");
    h.click("400%");
    assert!(h.plan().nearest, "400 % is magnified native pixels");
    assert!(h.plan().viewport_px.0 <= h.plan().display_px.0 / 3);

    h.click("400%");
    h.click("Fit");
    assert_eq!(h.plan().window, None, "Fit returns to the whole crop");
    assert!(!h.plan().nearest);
}

#[test]
fn a_tiny_photo_that_already_fits_above_the_ceiling_offers_fit_alone() {
    let mut h = Harness::new((300, 200));
    h.click("Fit");
    assert!(
        h.button("100%").is_none() && h.button("200%").is_none() && h.button("400%").is_none(),
        "nothing to zoom into"
    );
}
