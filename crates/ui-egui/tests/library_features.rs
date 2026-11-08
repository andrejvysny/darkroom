//! Headless Library features: multi-select, batch culling, delete-rejected confirmation, image
//! tags, smart-collection predicate and the empty-library call to action.

use core_app::import::ImportRequest;
use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, ImageTags, LibrarySnapshot,
};
use core_app::workflow::{Command, CullChange, Mutation};
use core_model::catalog::{ImageRow, KeywordRow, QueryParams, RejectSummary};
use core_model::identity::ImageId;
use core_model::DevelopParams;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use std::sync::Arc;
use ui_egui::{PhotoUi, ThumbSource, ThumbState, UiState};

fn snapshot(n: i64, selected: Option<i64>) -> ApplicationSnapshot {
    let rows: Vec<ImageRow> = (1..=n)
        .map(|id| ImageRow {
            id,
            filename: format!("IMG_{id:04}.cr3"),
            ..Default::default()
        })
        .collect();
    let image = selected.and_then(|id| rows.iter().find(|r| r.id == id).cloned());
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        library: LibrarySnapshot {
            count: n,
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

struct Thumbs;
impl ThumbSource for Thumbs {
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
    modifiers: Modifiers,
    /// Every string painted in the last frame.
    text: String,
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

impl Harness {
    fn new(snapshot: ApplicationSnapshot) -> Self {
        let mut h = Self {
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
                placeholder: None,
            },
            snapshot,
            time: 0.0,
            modifiers: Modifiers::NONE,
            text: String::new(),
        };
        for _ in 0..3 {
            h.frame(Vec::new());
        }
        h
    }

    fn frame(&mut self, mut events: Vec<Event>) -> Vec<Command> {
        self.time += 0.05;
        events.insert(0, Event::ModifiersChanged(self.modifiers));
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 700.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let (mut error, mut notices) = (None, Vec::new());
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
                    thumbs: &mut Thumbs,
                    histogram: None,
                    import: &core_app::import::ImportView::default(),
                    export: &core_app::export::ExportView::default(),
                },
                &mut self.ui,
            );
        });
        self.text.clear();
        for clipped in &output.shapes {
            collect_text(&clipped.shape, &mut self.text);
        }
        output.textures_delta.clear();
        commands
    }

    fn click_at(&mut self, pos: Pos2) -> Vec<Command> {
        let modifiers = self.modifiers;
        let button = move |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        };
        let mut seen = self.frame(vec![Event::PointerMoved(pos)]);
        seen.extend(self.frame(vec![button(true)]));
        seen.extend(self.frame(vec![button(false)]));
        seen
    }

    fn click_cell(&mut self, id: i64) -> Vec<Command> {
        let r = self
            .ui
            .library
            .cell_rects
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .expect("cell laid out");
        self.click_at(Pos2::new(r.center().x, r.top() + r.width() * 0.5))
    }

    fn click_button(&mut self, name: &str) -> Vec<Command> {
        let rect = self
            .ui
            .library
            .button_rects
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("button {name} not laid out"));
        self.click_at(rect.center())
    }
}

fn key(key: Key) -> Vec<Event> {
    vec![Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]
}

fn selected(h: &Harness) -> Vec<i64> {
    h.ui.library.selected.iter().copied().collect()
}

fn mutations(commands: &[Command]) -> Vec<&Mutation> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::Mutate(m) => Some(m),
            _ => None,
        })
        .collect()
}

#[test]
fn click_cmd_click_and_shift_click_build_the_selection() {
    let mut h = Harness::new(snapshot(20, Some(1)));
    let seen = h.click_cell(1);
    assert!(
        seen.contains(&Command::SelectImage(Some(ImageId(1)))),
        "{seen:?}"
    );
    assert_eq!(selected(&h), vec![1]);

    h.modifiers = Modifiers::COMMAND;
    let seen = h.click_cell(3);
    assert!(
        seen.contains(&Command::SelectImage(Some(ImageId(3)))),
        "{seen:?}"
    );
    assert_eq!(selected(&h), vec![1, 3]);

    h.modifiers = Modifiers::SHIFT;
    h.click_cell(6);
    assert_eq!(
        selected(&h),
        vec![3, 4, 5, 6],
        "range from the cmd-click anchor"
    );

    h.modifiers = Modifiers::COMMAND;
    h.click_cell(4);
    assert_eq!(selected(&h), vec![3, 5, 6]);

    h.modifiers = Modifiers::NONE;
    h.click_cell(9);
    assert_eq!(selected(&h), vec![9]);
}

#[test]
fn batch_key_emits_one_cull_mutation_without_advancing() {
    let mut h = Harness::new(snapshot(20, Some(1)));
    h.click_cell(1);
    h.modifiers = Modifiers::COMMAND;
    h.click_cell(2);
    h.modifiers = Modifiers::NONE;
    let seen = h.frame(key(Key::Num3));
    assert_eq!(
        mutations(&seen),
        vec![&Mutation::Cull {
            images: vec![ImageId(1), ImageId(2)],
            change: CullChange::Rating(3),
        }],
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|c| matches!(c, Command::SelectImage(_) | Command::SetCull { .. })),
        "{seen:?}"
    );
}

#[test]
fn escape_collapses_to_the_primary() {
    let mut h = Harness::new(snapshot(20, Some(1)));
    h.modifiers = Modifiers::COMMAND;
    h.click_cell(1);
    h.click_cell(2);
    assert_eq!(selected(&h).len(), 2);
    h.modifiers = Modifiers::NONE;
    h.frame(key(Key::Escape));
    assert_eq!(selected(&h), vec![1]);
}

fn reject_snapshot() -> ApplicationSnapshot {
    let mut s = snapshot(5, None);
    s.library.query.flag = Some("reject".into());
    s
}

#[test]
fn delete_rejected_needs_two_steps_and_carries_the_captured_query() {
    let mut h = Harness::new(reject_snapshot());
    let seen = h.click_button("delete_rejected");
    let query = Box::new(h.snapshot.library.query.clone());
    assert!(
        seen.contains(&Command::LoadRejectSummary(query.clone())),
        "{seen:?}"
    );
    // No counts for this query yet: no destructive buttons.
    h.frame(Vec::new());
    assert!(h
        .ui
        .library
        .button_rects
        .iter()
        .all(|(n, _)| *n != "trash_step1"));
    // A summary for a different query is ignored.
    let other = QueryParams {
        flag: Some("pick".into()),
        ..Default::default()
    };
    h.snapshot.library.reject_summary = Some((other, RejectSummary::default()));
    h.frame(Vec::new());
    assert!(h
        .ui
        .library
        .button_rects
        .iter()
        .all(|(n, _)| *n != "trash_step1"));

    h.snapshot.library.reject_summary = Some((
        (*query).clone(),
        RejectSummary {
            images: 2,
            companions: 1,
            bytes: 3_000_000,
        },
    ));
    h.frame(Vec::new());
    assert!(h.text.contains("2 photos"), "{}", h.text);
    let seen = h.click_button("trash_step1");
    assert!(mutations(&seen).is_empty(), "{seen:?}");
    h.frame(Vec::new());
    let seen = h.click_button("trash_confirm");
    assert_eq!(
        mutations(&seen),
        vec![&Mutation::DeleteRejected(query)],
        "{seen:?}"
    );
    h.frame(Vec::new());
    assert!(h
        .ui
        .library
        .button_rects
        .iter()
        .all(|(n, _)| *n != "trash_confirm"));
}

fn tags_for(image: i64, keyword: &str) -> Arc<ImageTags> {
    Arc::new(ImageTags {
        image: ImageId(image),
        keywords: vec![KeywordRow {
            id: 1,
            name: keyword.into(),
            count: 1,
        }],
        ..Default::default()
    })
}

#[test]
fn tags_of_another_image_are_not_rendered() {
    let mut s = snapshot(5, Some(2));
    s.library.tags = Some(tags_for(1, "otherkw"));
    let mut h = Harness::new(s);
    assert!(!h.text.contains("otherkw"), "{}", h.text);
    h.snapshot.library.tags = Some(tags_for(2, "ownkw"));
    h.frame(Vec::new());
    assert!(h.text.contains("ownkw"), "{}", h.text);
}

#[test]
fn load_image_tags_once_per_image_and_revision() {
    let mut h = Harness::new(snapshot(5, Some(2)));
    // The harness warm-up already consumed the first request for (2, 0).
    assert!(!h
        .frame(Vec::new())
        .contains(&Command::LoadImageTags(ImageId(2))));
    h.snapshot.library.tags_revision = 1;
    let seen = h.frame(Vec::new());
    assert_eq!(
        seen.iter()
            .filter(|c| **c == Command::LoadImageTags(ImageId(2)))
            .count(),
        1
    );
    assert!(!h
        .frame(Vec::new())
        .contains(&Command::LoadImageTags(ImageId(2))));
    h.snapshot.editor.selected = Some(ImageId(3));
    let seen = h.frame(Vec::new());
    assert!(
        seen.contains(&Command::LoadImageTags(ImageId(3))),
        "{seen:?}"
    );
}

#[test]
fn saving_a_smart_collection_captures_only_the_predicate_dims() {
    let mut s = snapshot(5, None);
    s.library.query = QueryParams {
        min_stars: Some(3),
        format: Some("raw".into()),
        capture_year: Some("2026".into()),
        sort: Some("filename".into()),
        ..Default::default()
    };
    let mut h = Harness::new(s);
    h.click_button("save_smart");
    h.frame(Vec::new());
    h.frame(vec![Event::Text("Fives".into())]);
    let seen = h.frame(key(Key::Enter));
    let expected = QueryParams {
        min_stars: Some(3),
        ..Default::default()
    };
    assert_eq!(
        mutations(&seen),
        vec![&Mutation::CreateCollection {
            name: "Fives".into(),
            smart: Some(Box::new(expected)),
            images: Vec::new(),
        }],
        "{seen:?}"
    );
}

#[test]
fn empty_library_offers_add_photos() {
    let mut h = Harness::new(snapshot(0, None));
    assert!(h.text.contains("Your library is empty"), "{}", h.text);
    h.click_button("add_photos");
    assert_eq!(h.ui.import_requests, vec![ImportRequest::Open]);
}

/// A populated catalog whose current query (`narrow`) matched nothing.
fn no_match(narrow: impl FnOnce(&mut QueryParams)) -> Harness {
    let mut snapshot = snapshot(0, None);
    narrow(&mut snapshot.library.query);
    Harness::new(snapshot)
}

fn requeried(commands: &[Command]) -> Vec<&QueryParams> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::QueryLibrary(q) => Some(&**q),
            _ => None,
        })
        .collect()
}

#[test]
fn search_only_zero_matches_is_not_an_empty_library() {
    // Regression DARKROOM-168: `any_filter` leaves search out, so a search that matched nothing
    // used to show the empty-catalog onboarding.
    let mut h = no_match(|q| q.search = Some("no-match-5px".into()));
    assert!(!h.text.contains("Your library is empty"), "{}", h.text);
    assert!(
        h.text
            .contains("No photos match \u{201c}no-match-5px\u{201d}."),
        "{}",
        h.text
    );
    assert!(!h
        .ui
        .library
        .button_rects
        .iter()
        .any(|(n, _)| *n == "add_photos" || *n == "clear_filters"));
    h.ui.library.search = "no-match-5px".into();
    let seen = h.click_button("clear_search");
    let queries = requeried(&seen);
    assert_eq!(queries.len(), 1, "{seen:?}");
    assert_eq!(queries[0].search, None);
    assert!(h.ui.library.search.is_empty());
    assert!(h.ui.import_requests.is_empty());
}

#[test]
fn filter_only_zero_matches_offers_clear_filters_not_onboarding() {
    let mut h = no_match(|q| q.min_stars = Some(5));
    assert!(
        h.text.contains("No photos match the current filters."),
        "{}",
        h.text
    );
    assert!(!h
        .ui
        .library
        .button_rects
        .iter()
        .any(|(n, _)| *n == "clear_search"));
    let seen = h.click_button("clear_filters");
    let queries = requeried(&seen);
    assert_eq!(queries.len(), 1, "{seen:?}");
    assert_eq!(queries[0].min_stars, None);
}

#[test]
fn search_and_filter_zero_matches_offers_both_and_keeps_the_other_dimension() {
    let mut h = no_match(|q| {
        q.search = Some("sunset".into());
        q.flag = Some("pick".into());
    });
    assert!(
        h.text
            .contains("No photos match \u{201c}sunset\u{201d} with the current filters."),
        "{}",
        h.text
    );
    let seen = h.click_button("clear_search");
    let queries = requeried(&seen);
    assert_eq!(
        (queries[0].search.as_deref(), queries[0].flag.as_deref()),
        (None, Some("pick"))
    );
    let seen = h.click_button("clear_filters");
    let queries = requeried(&seen);
    assert_eq!(queries[0].flag, None);
    assert_eq!(queries[0].search.as_deref(), Some("sunset"));
}

#[test]
fn enter_that_ends_a_text_edit_is_not_a_shortcut() {
    // Regression (live QA): the TextEdit surrenders focus while handling Enter, and the global
    // Library keys then read the same Enter as "open Develop".
    let mut s = snapshot(5, Some(2));
    s.library.query = QueryParams {
        min_stars: Some(3),
        ..Default::default()
    };
    let mut h = Harness::new(s);
    h.click_button("save_smart");
    h.frame(Vec::new());
    h.frame(vec![Event::Text("Fives".into())]);
    let seen = h.frame(key(Key::Enter));
    assert_eq!(mutations(&seen).len(), 1, "{seen:?}");
    assert_eq!(h.ui.mode, ui_egui::Mode::Library);
}

#[test]
fn culling_keys_are_ignored_while_a_modal_is_open() {
    // Regression (review): egui modals block the pointer only, so `X` under the delete-rejected
    // confirmation used to reject the grid selection behind it.
    let mut s = reject_snapshot();
    s.editor.selected = Some(ImageId(2));
    s.editor.image = s.library.rows.iter().find(|r| r.id == 2).cloned();
    let mut h = Harness::new(s);
    h.click_button("delete_rejected");
    h.frame(Vec::new());
    let seen = h.frame(key(Key::X));
    assert!(
        !seen.iter().any(|c| matches!(
            c,
            Command::SetCull { .. } | Command::Mutate(Mutation::Cull { .. })
        )),
        "{seen:?}"
    );
    let mut h = Harness::new(snapshot(5, Some(2)));
    h.ui.import_requests.clear();
    let seen = h.frame(key(Key::X));
    assert!(
        seen.iter().any(|c| matches!(c, Command::SetCull { .. })),
        "control: X culls without a modal: {seen:?}"
    );
}

impl Harness {
    fn right_click_cell(&mut self, id: i64) -> Vec<Command> {
        let r = self
            .ui
            .library
            .cell_rects
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .expect("cell laid out");
        let pos = Pos2::new(r.center().x, r.top() + r.width() * 0.5);
        let modifiers = self.modifiers;
        let button = move |pressed| Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers,
        };
        let mut seen = self.frame(vec![Event::PointerMoved(pos)]);
        seen.extend(self.frame(vec![button(true)]));
        seen.extend(self.frame(vec![button(false)]));
        seen.extend(self.frame(Vec::new()));
        seen
    }
}

fn remove_cmd(ids: &[i64]) -> Mutation {
    Mutation::RemoveFromLibrary {
        images: ids.iter().map(|i| ImageId(*i)).collect(),
    }
}

#[test]
fn remove_from_library_uses_the_selection_or_the_clicked_row() {
    let mut h = Harness::new(snapshot(20, Some(1)));
    h.click_cell(1);
    h.modifiers = Modifiers::COMMAND;
    h.click_cell(2);
    h.modifiers = Modifiers::NONE;

    // Right-click inside the multi-selection: the whole selection, nothing sent until confirm.
    let seen = h.right_click_cell(2);
    assert!(mutations(&seen).is_empty(), "{seen:?}");
    let seen = h.click_button("remove_menu_item");
    assert!(mutations(&seen).is_empty(), "{seen:?}");
    h.frame(Vec::new());
    assert!(
        h.text.contains("Remove 2 photos from the library?"),
        "{}",
        h.text
    );
    assert!(h.text.contains("The files stay on disk"), "{}", h.text);
    let seen = h.click_button("remove_confirm");
    assert_eq!(mutations(&seen), vec![&remove_cmd(&[1, 2])], "{seen:?}");
    assert!(selected(&h).is_empty(), "{:?}", selected(&h));
    h.frame(Vec::new());
    assert!(h
        .ui
        .library
        .button_rects
        .iter()
        .all(|(n, _)| *n != "remove_confirm"));

    // Right-click outside the selection: just that row, selection untouched.
    let mut h = Harness::new(snapshot(20, Some(1)));
    h.click_cell(1);
    h.right_click_cell(5);
    h.click_button("remove_menu_item");
    h.frame(Vec::new());
    assert!(
        h.text.contains("Remove 1 photo from the library?"),
        "{}",
        h.text
    );
    let seen = h.click_button("remove_confirm");
    assert_eq!(mutations(&seen), vec![&remove_cmd(&[5])], "{seen:?}");
    assert_eq!(selected(&h), vec![1]);
}

#[test]
fn remove_dialog_cancel_and_escape_emit_nothing_and_block_culling_keys() {
    let mut h = Harness::new(snapshot(20, Some(2)));
    h.click_cell(2);
    h.right_click_cell(2);
    h.click_button("remove_menu_item");
    h.frame(Vec::new());
    let seen = h.frame(key(Key::X));
    assert!(
        !seen.iter().any(|c| matches!(
            c,
            Command::SetCull { .. } | Command::Mutate(Mutation::Cull { .. })
        )),
        "{seen:?}"
    );
    let seen = h.click_button("remove_cancel");
    assert!(mutations(&seen).is_empty(), "{seen:?}");
    h.frame(Vec::new());
    assert!(h
        .ui
        .library
        .button_rects
        .iter()
        .all(|(n, _)| *n != "remove_confirm"));

    h.right_click_cell(2);
    h.click_button("remove_menu_item");
    h.frame(Vec::new());
    let seen = h.frame(key(Key::Escape));
    assert!(mutations(&seen).is_empty(), "{seen:?}");
    h.frame(Vec::new());
    assert!(h
        .ui
        .library
        .button_rects
        .iter()
        .all(|(n, _)| *n != "remove_confirm"));
}
