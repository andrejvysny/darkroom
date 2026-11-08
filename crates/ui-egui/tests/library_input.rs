//! Headless Library-mode input: synthetic pointer/keyboard events drive `ui_egui::root` over a
//! snapshot with library rows; emitted `Command`s and virtualisation are asserted.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, LibrarySnapshot,
};
use core_app::workflow::{Command, CullChange, CullFlag};
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use core_model::DevelopParams;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, ThumbSource, ThumbState, UiState};

fn rows(n: i64) -> Vec<ImageRow> {
    (1..=n)
        .map(|id| ImageRow {
            id,
            filename: format!("IMG_{id:04}.cr3"),
            ..Default::default()
        })
        .collect()
}

fn snapshot(n: i64, selected: Option<i64>) -> ApplicationSnapshot {
    let rows = rows(n);
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

/// Records every row the UI asked a thumbnail for.
#[derive(Default)]
struct Recorder(Vec<i64>);
impl ThumbSource for Recorder {
    fn thumb(&mut self, image: &ImageRow) -> ThumbState {
        self.0.push(image.id);
        ThumbState::Loading
    }
}

struct Harness {
    ctx: egui::Context,
    ui: UiState,
    photo: PhotoUi,
    snapshot: ApplicationSnapshot,
    thumbs: Recorder,
    time: f64,
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
            thumbs: Recorder::default(),
            time: 0.0,
        };
        for _ in 0..3 {
            h.frame(Vec::new());
        }
        h
    }

    fn frame(&mut self, events: Vec<Event>) -> Vec<Command> {
        self.time += 0.05;
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 700.0))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        let reopen = Command::RefreshLibrary;
        let (mut error, mut notices) = (None, Vec::new());
        self.thumbs.0.clear();
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
                    thumbs: &mut self.thumbs,
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

    fn cell(&self, id: i64) -> Rect {
        self.ui
            .library
            .cell_rects
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .expect("cell laid out")
    }

    fn click(&mut self, pos: Pos2) -> Vec<Command> {
        let mut seen = self.frame(vec![Event::PointerMoved(pos)]);
        seen.extend(self.frame(vec![button(pos, true)]));
        seen.extend(self.frame(vec![button(pos, false)]));
        seen
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

fn key(key: Key) -> Vec<Event> {
    vec![Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]
}

fn thumb_center(h: &Harness, id: i64) -> Pos2 {
    let r = h.cell(id);
    Pos2::new(r.center().x, r.top() + r.width() * 0.5)
}

fn set_cull(commands: &[Command]) -> Vec<&CullChange> {
    commands
        .iter()
        .filter_map(|c| match c {
            Command::SetCull { change, .. } => Some(change),
            _ => None,
        })
        .collect()
}

#[test]
fn click_selects_and_double_click_opens_loupe() {
    let mut h = Harness::new(snapshot(20, None));
    let at = thumb_center(&h, 1);
    let seen = h.click(at);
    assert!(
        seen.contains(&Command::SelectImage(Some(ImageId(1)))),
        "{seen:?}"
    );
    assert_eq!(h.ui.mode, Mode::Library);
    let mut seen = h.click(at);
    seen.extend(h.click(at));
    assert_eq!(h.ui.mode, Mode::Library, "{seen:?}");
    assert!(h.ui.library.loupe, "{seen:?}");
}

#[test]
fn culling_keys_emit_set_cull_for_the_selection() {
    let mut h = Harness::new(snapshot(20, Some(2)));
    let seen = h.frame(key(Key::Num3));
    assert_eq!(set_cull(&seen), vec![&CullChange::Rating(3)], "{seen:?}");
    let seen = h.frame(key(Key::P));
    assert_eq!(
        set_cull(&seen),
        vec![&CullChange::Flag(CullFlag::Pick)],
        "{seen:?}"
    );
    // Rating advances the selection like the legacy culling hook.
    let seen = h.frame(key(Key::Num3));
    assert!(
        seen.contains(&Command::SelectImage(Some(ImageId(3)))),
        "{seen:?}"
    );
}

#[test]
fn keys_do_nothing_without_a_selection() {
    let mut h = Harness::new(snapshot(20, None));
    assert!(h.frame(key(Key::Num3)).is_empty());
}

#[test]
fn keys_are_ignored_while_the_search_box_has_focus() {
    let mut h = Harness::new(snapshot(20, Some(2)));
    let id = egui::Id::unique(ui_egui::SEARCH_ID);
    h.ctx.memory_mut(|m| m.request_focus(id));
    h.frame(Vec::new());
    let seen = h.frame(key(Key::Num3));
    assert!(set_cull(&seen).is_empty(), "{seen:?}");
}

#[test]
fn picks_nav_requeries_with_flag_and_reset_paging() {
    let mut h = Harness::new(snapshot(20, None));
    // Find the "Picks" row by scanning the nav column for the command it produces.
    let mut found = None;
    let mut y = 80.0;
    while y < 400.0 && found.is_none() {
        let seen = h.click(Pos2::new(60.0, y));
        found = seen.into_iter().find_map(|c| match c {
            Command::QueryLibrary(q) if q.flag.as_deref() == Some("pick") => Some(q),
            _ => None,
        });
        y += 8.0;
    }
    let query = found.expect("a nav row requests flag=pick");
    assert_eq!(query.offset, Some(0));
    assert_eq!(query.cursor_value, None);
}

#[test]
fn grid_only_asks_thumbs_for_visible_rows() {
    let mut h = Harness::new(snapshot(3000, None));
    h.frame(Vec::new());
    let asked = h.thumbs.0.len();
    assert!(asked > 0 && asked < 200, "asked {asked}");
    let laid_out: Vec<i64> = h.ui.library.cell_rects.iter().map(|(i, _)| *i).collect();
    // The info panel preview asks nothing (no selection): exactly the laid-out cells, in order.
    assert_eq!(h.thumbs.0, laid_out);
    assert!(laid_out.windows(2).all(|w| w[0] < w[1]));
    assert!(laid_out.iter().all(|id| *id <= 100), "{laid_out:?}");
}

#[test]
fn g_and_d_switch_modes() {
    let mut h = Harness::new(snapshot(5, Some(1)));
    h.frame(key(Key::D));
    assert_eq!(h.ui.mode, Mode::Develop);
    h.frame(key(Key::G));
    assert_eq!(h.ui.mode, Mode::Library);

    let mut h = Harness::new(snapshot(5, None));
    h.frame(key(Key::D));
    assert_eq!(h.ui.mode, Mode::Library, "D needs a selection");
}
