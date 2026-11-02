//! Headless Import dialog: selection rules, Commit capture, virtualisation and the progress pill,
//! driven through `ui_egui::root` with a synthetic `ImportView`.

use core_app::import::{
    FileStatus, ImportFile, ImportMode, ImportPhase, ImportRequest, ImportView,
};
use core_app::state::ApplicationSnapshot;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use std::path::PathBuf;
use std::sync::Arc;
use ui_egui::{NoThumbs, PhotoUi, UiState};

fn file(name: &str, status: FileStatus, kind: &str) -> ImportFile {
    ImportFile {
        path: format!("/src/{name}"),
        filename: name.to_owned(),
        size_bytes: 4096,
        mtime: 1_700_000_000,
        status,
        kind: kind.to_owned(),
        ..Default::default()
    }
}

fn view(files: Vec<ImportFile>, generation: u64) -> ImportView {
    ImportView {
        open: true,
        source: Some(PathBuf::from("/src")),
        recursive: true,
        files: Arc::new(files),
        dedup: (0, 0),
        phase: ImportPhase::Ready,
        generation,
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
    fn new() -> Self {
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
            snapshot: ApplicationSnapshot {
                revision: 1,
                running: true,
                catalog_ready: true,
                library: Default::default(),
                editor: Default::default(),
                catalog_epoch: Default::default(),
                jobs: Default::default(),
                settings: Default::default(),
                error: None,
            },
            time: 0.0,
        }
    }

    fn frame(&mut self, view: &ImportView, events: Vec<Event>) -> Vec<ImportRequest> {
        self.time += 0.05;
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
                    import: view,
                },
                &mut self.ui,
            );
        });
        output.textures_delta.clear();
        std::mem::take(&mut self.ui.import_requests)
    }

    fn settle(&mut self, view: &ImportView) {
        for _ in 0..3 {
            self.frame(view, Vec::new());
        }
    }

    fn click(&mut self, view: &ImportView, pos: Pos2) -> Vec<ImportRequest> {
        let mut seen = self.frame(view, vec![Event::PointerMoved(pos)]);
        seen.extend(self.frame(view, vec![button(pos, true)]));
        seen.extend(self.frame(view, vec![button(pos, false)]));
        seen
    }

    fn click_import(&mut self, view: &ImportView) -> Vec<ImportRequest> {
        let rect = self
            .ui
            .import
            .import_button
            .expect("import button laid out");
        self.click(view, rect.center())
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

fn commits(requests: &[ImportRequest]) -> Vec<&core_app::import::ImportPlan> {
    requests
        .iter()
        .filter_map(|r| match r {
            ImportRequest::Commit(plan) => Some(&**plan),
            _ => None,
        })
        .collect()
}

#[test]
fn generation_change_resets_selection_to_new_files() {
    let mut h = Harness::new();
    let v1 = view(
        vec![
            file("a.cr3", FileStatus::New, "raw"),
            file("b.cr3", FileStatus::DuplicateLibrary, "raw"),
            file("c.cr3", FileStatus::Pending, "raw"),
        ],
        1,
    );
    h.settle(&v1);
    assert!(h.ui.import.is_selected("/src/a.cr3"));
    assert!(h.ui.import.is_selected("/src/c.cr3"));
    assert!(!h.ui.import.is_selected("/src/b.cr3"));
    h.ui.import.select_none(&v1);
    assert_eq!(h.ui.import.selected_count(), 0);
    // Same generation, new Arc (dedup results): the user's choice stands.
    let v1b = view((*v1.files).clone(), 1);
    h.settle(&v1b);
    assert_eq!(h.ui.import.selected_count(), 0);
    let v2 = view((*v1.files).clone(), 2);
    h.settle(&v2);
    assert_eq!(h.ui.import.selected_count(), 2);
}

#[test]
fn only_new_and_skip_duplicates_exclude_duplicates() {
    let mut h = Harness::new();
    let v = view(
        vec![
            file("a.cr3", FileStatus::New, "raw"),
            file("b.cr3", FileStatus::DuplicateLibrary, "raw"),
            file("c.cr3", FileStatus::DuplicateBatch, "raw"),
        ],
        1,
    );
    h.settle(&v);
    assert_eq!(h.ui.import.selected_count(), 1);
    // Skip on: duplicates are unselectable.
    h.ui.import.select_all(&v);
    assert_eq!(h.ui.import.selected_count(), 1);
    // Skip off: everything selectable; Only new drops duplicates again.
    h.ui.import.set_skip_duplicates(&v, false);
    h.ui.import.select_all(&v);
    assert_eq!(h.ui.import.selected_count(), 3);
    h.ui.import.select_only_new(&v);
    assert_eq!(h.ui.import.selected_count(), 1);
    // Re-enabling skip deselects a manually chosen duplicate.
    h.ui.import.select_all(&v);
    h.ui.import.set_skip_duplicates(&v, true);
    assert_eq!(h.ui.import.selected_count(), 1);
    assert!(h.ui.import.is_selected("/src/a.cr3"));
}

fn pair_files() -> Vec<ImportFile> {
    let mut raw = file("IMG_1.cr3", FileStatus::New, "raw");
    raw.pair_key = Some("IMG_1".into());
    raw.pair_role = Some("primary".into());
    let mut jpg = file("IMG_1.jpg", FileStatus::New, "jpeg");
    jpg.pair_key = Some("IMG_1".into());
    jpg.pair_role = Some("secondary".into());
    vec![raw, jpg, file("solo.cr3", FileStatus::New, "raw")]
}

#[test]
fn pair_partners_toggle_together() {
    let mut h = Harness::new();
    let v = view(pair_files(), 1);
    h.settle(&v);
    assert_eq!(h.ui.import.selected_count(), 3);
    assert_eq!(h.ui.import.selected_pairs(&v), (1, 2));
    h.ui.import.toggle_path(&v, "/src/IMG_1.jpg", false);
    assert!(!h.ui.import.is_selected("/src/IMG_1.cr3"));
    assert!(!h.ui.import.is_selected("/src/IMG_1.jpg"));
    assert!(h.ui.import.is_selected("/src/solo.cr3"));
    h.ui.import.toggle_path(&v, "/src/IMG_1.cr3", true);
    assert!(h.ui.import.is_selected("/src/IMG_1.jpg"));
}

#[test]
fn commit_carries_full_selection_mode_and_destination() {
    let mut h = Harness::new();
    let mut v = view(pair_files(), 1);
    v.library_root = Some(PathBuf::from("/lib"));
    h.settle(&v);
    // Filter the visible list to RAW only; the JPEG stays selected and is still committed.
    h.ui.import.kind_filter = Some("raw".into());
    h.ui.import.mode = ImportMode::Copy;
    h.settle(&v);
    assert!(h.ui.import.import_enabled);
    let requests = h.click_import(&v);
    let plans = commits(&requests);
    assert_eq!(plans.len(), 1, "{requests:?}");
    let plan = plans[0];
    assert_eq!(
        plan.selected,
        ["/src/IMG_1.cr3", "/src/IMG_1.jpg", "/src/solo.cr3"]
    );
    assert_eq!(plan.mode, ImportMode::Copy);
    assert_eq!(plan.destination, Some(PathBuf::from("/lib")));
    assert_eq!(plan.source, PathBuf::from("/src"));
}

#[test]
fn reference_commit_has_no_destination() {
    let mut h = Harness::new();
    let v = view(vec![file("a.cr3", FileStatus::New, "raw")], 1);
    h.settle(&v);
    let requests = h.click_import(&v);
    let plans = commits(&requests);
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].mode, ImportMode::Reference);
    assert_eq!(plans[0].destination, None);
}

#[test]
fn import_disabled_without_selection_or_destination() {
    let mut h = Harness::new();
    let v = view(vec![file("a.cr3", FileStatus::New, "raw")], 1);
    h.settle(&v);
    h.ui.import.select_none(&v);
    h.settle(&v);
    assert!(!h.ui.import.import_enabled);
    assert!(commits(&h.click_import(&v)).is_empty());

    // Copy with a selection but no library root / chosen destination.
    h.ui.import.select_all(&v);
    h.ui.import.mode = ImportMode::Copy;
    h.settle(&v);
    assert!(!h.ui.import.import_enabled);
    assert!(commits(&h.click_import(&v)).is_empty());

    let mut with_dest = v.clone();
    with_dest.destination = Some(PathBuf::from("/dest"));
    h.settle(&with_dest);
    assert!(h.ui.import.import_enabled);
}

#[test]
fn list_is_virtualised_for_10k_files() {
    let mut h = Harness::new();
    let files: Vec<ImportFile> = (0..10_000)
        .map(|i| {
            let mut f = file(&format!("IMG_{i:05}.cr3"), FileStatus::New, "raw");
            f.mtime = 1_700_000_000 + (i / 100) * 86_400;
            f
        })
        .collect();
    let v = view(files, 1);
    h.settle(&v);
    let rows = h.ui.import.laid_out_rows;
    assert!(rows > 0 && rows < 80, "laid out {rows} rows");
    assert_eq!(h.ui.import.selected_count(), 10_000);
}

#[test]
fn esc_closes_dialog() {
    let mut h = Harness::new();
    let v = view(vec![file("a.cr3", FileStatus::New, "raw")], 1);
    h.settle(&v);
    let requests = h.frame(
        &v,
        vec![Event::Key {
            key: Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        }],
    );
    assert!(requests.contains(&ImportRequest::Close), "{requests:?}");
}

#[test]
fn pill_shown_when_closed_and_busy() {
    let mut h = Harness::new();
    let mut v = view(Vec::new(), 1);
    v.open = false;
    v.phase = ImportPhase::Importing { done: 3, total: 10 };
    h.settle(&v);
    assert!(h.ui.import.pill_rect.is_some());
    assert!(!h.ui.import.dialog_shown);
    v.open = true;
    h.settle(&v);
    assert!(h.ui.import.pill_rect.is_none());
    v.open = false;
    v.phase = ImportPhase::Idle;
    h.settle(&v);
    assert!(h.ui.import.pill_rect.is_none());
}

#[test]
fn default_selection_comes_from_the_listing_result_not_the_empty_listing_frame() {
    // Regression (live QA): the generation advances when listing starts; the empty list seen
    // during `Listing` must not consume the "select every new file" reset.
    let mut h = Harness::new();
    let mut listing = view(Vec::new(), 1);
    listing.phase = ImportPhase::Listing;
    h.settle(&listing);
    assert_eq!(h.ui.import.selected_count(), 0);
    let listed = view(
        vec![
            file("a.cr3", FileStatus::Pending, "raw"),
            file("b.jpg", FileStatus::Pending, "jpeg"),
        ],
        1,
    );
    h.settle(&listed);
    assert_eq!(h.ui.import.selected_count(), 2);
}
