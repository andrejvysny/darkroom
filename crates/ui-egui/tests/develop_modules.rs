//! Headless egui input for the Develop modules: sliders, module resets, colour mixer, tone curve
//! and filmstrip. Synthetic pointer/keyboard events drive `ui_egui::root` over a Ready snapshot
//! and the emitted `Command`s are asserted. No window, no GPU.

use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SaveStatus, SettingsSnapshot,
};
use core_app::workflow::{Command, CullChange, EditIntent};
use core_model::catalog::ImageRow;
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect};
use std::sync::Arc;
use ui_egui::{Mode, PhotoUi, ThumbSource, ThumbState, UiState};

fn snapshot(params: DevelopParams) -> ApplicationSnapshot {
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
                params: Some(Arc::new(params)),
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

struct Counting(usize);
impl ThumbSource for Counting {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        self.0 += 1;
        ThumbState::Loading
    }
}

struct Harness {
    ctx: egui::Context,
    ui: UiState,
    photo: PhotoUi,
    snapshot: ApplicationSnapshot,
    thumbs: Counting,
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
            thumbs: Counting(0),
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
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 2600.0))),
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

    fn track(&self, name: &str) -> Rect {
        *self
            .ui
            .develop
            .slider_tracks
            .get(name)
            .unwrap_or_else(|| panic!("slider {name} not laid out"))
    }

    fn value_rect(&self, name: &str) -> Rect {
        self.ui.develop.slider_values[name]
    }

    fn click(&mut self, pos: Pos2) -> Vec<Command> {
        let mut seen = self.frame(vec![Event::PointerMoved(pos)]);
        seen.extend(self.frame(vec![button(pos, true)]));
        seen.extend(self.frame(vec![button(pos, false)]));
        seen
    }

    /// Press, move through `to`, release; returns every command seen.
    fn drag(&mut self, from: Pos2, to: &[Pos2]) -> Vec<Command> {
        let mut seen = self.frame(vec![Event::PointerMoved(from)]);
        seen.extend(self.frame(vec![button(from, true)]));
        for p in to {
            seen.extend(self.frame(vec![Event::PointerMoved(*p)]));
        }
        let end = *to.last().unwrap();
        seen.extend(self.frame(vec![button(end, false)]));
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

fn key(key: Key) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }
}

fn at(rect: Rect, fraction: f32) -> Pos2 {
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

fn applied(commands: &[Command]) -> Vec<&DevelopParams> {
    edits(commands)
        .into_iter()
        .filter_map(|i| match i {
            EditIntent::Apply(p) => Some(&**p),
            _ => None,
        })
        .collect()
}

#[test]
fn click_on_the_track_lands_near_the_pointer_not_at_the_minimum() {
    // Regression DARKROOM-84: a click without drag used to jump to the range minimum.
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let track = h.track("contrast");
    let seen = h.click(at(track, 0.8));
    let all = applied(&seen);
    assert_eq!(all.len(), 1, "{seen:?}");
    assert!((all[0].contrast - 60.0).abs() < 3.0, "{}", all[0].contrast);
    assert!(edits(&seen)
        .iter()
        .all(|i| !matches!(i, EditIntent::BeginGesture)));
}

#[test]
fn drag_on_highlights_streams_only_that_field() {
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let track = h.track("highlights");
    let seen = h.drag(
        at(track, 0.5),
        &[at(track, 0.6), at(track, 0.7), at(track, 0.2)],
    );
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
    let updates: Vec<&DevelopParams> = intents
        .iter()
        .filter_map(|i| match i {
            EditIntent::UpdateGesture(p) => Some(&**p),
            _ => None,
        })
        .collect();
    assert!(updates.len() >= 2, "{intents:?}");
    let base = DevelopParams::default();
    for u in &updates {
        assert!(u.highlights != 0.0);
        let mut rest = (*u).clone();
        rest.highlights = base.highlights;
        assert_eq!(rest, base, "only highlights may change");
    }
    assert!(
        updates.last().unwrap().highlights < 0.0,
        "ends left of centre"
    );
}

#[test]
fn double_click_resets_the_slider() {
    let params = DevelopParams {
        blacks: 40.0,
        ..Default::default()
    };
    let mut h = Harness::new(snapshot(params));
    let pos = at(h.track("blacks"), 0.7);
    let mut seen = h.click(pos);
    seen.extend(h.click(pos));
    let last = applied(&seen).into_iter().last().expect("an Apply");
    assert_eq!(last.blacks, 0.0);
}

#[test]
fn module_reset_applies_once_and_only_touches_its_keys() {
    let params = DevelopParams {
        exposure: 1.5,
        contrast: 20.0,
        blacks: -30.0,
        sharpen: 33.0,
        temp: 12.0,
        ..Default::default()
    };
    let mut h = Harness::new(snapshot(params));
    let reset = h.ui.develop.module_resets["light"];
    let seen = h.click(reset.center());
    let all = applied(&seen);
    assert_eq!(all.len(), 1, "{seen:?}");
    let p = all[0];
    assert_eq!((p.exposure, p.contrast, p.blacks), (0.0, 0.0, 0.0));
    assert_eq!((p.sharpen, p.temp), (33.0, 12.0), "other modules untouched");
}

#[test]
fn tone_curve_reset_restores_base_curve_and_curve_points() {
    let params = DevelopParams {
        tone_amount: 40.0,
        tone_curve: core_model::ToneCurve {
            rgb: vec![
                core_model::CurvePoint { x: 0.0, y: 0.0 },
                core_model::CurvePoint { x: 0.5, y: 0.7 },
                core_model::CurvePoint { x: 1.0, y: 1.0 },
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut h = Harness::new(snapshot(params));
    let reset = h.ui.develop.module_resets["curve"];
    let seen = h.click(reset.center());
    let all = applied(&seen);
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].tone_amount, 100.0);
    assert!(all[0].tone_curve.rgb.is_empty());
}

#[test]
fn numeric_entry_applies_on_enter_and_clamps_to_the_range() {
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let value = h.value_rect("shadows");
    h.click(value.center());
    h.frame(Vec::new());
    h.frame(Vec::new());
    h.frame(vec![Event::Text("150".into())]);
    let seen = h.frame(vec![key(Key::Enter)]);
    let all = applied(&seen);
    assert_eq!(all.len(), 1, "{seen:?}");
    assert_eq!(all[0].shadows, 100.0, "150 clamps to the slider range");

    let value = h.value_rect("whites");
    h.click(value.center());
    h.frame(Vec::new());
    h.frame(Vec::new());
    h.frame(vec![Event::Text("-25".into())]);
    let seen = h.frame(vec![key(Key::Enter)]);
    assert_eq!(
        applied(&seen).first().map(|p| p.whites),
        Some(-25.0),
        "{seen:?}"
    );
}

#[test]
fn escape_cancels_numeric_entry() {
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let value = h.value_rect("shadows");
    h.click(value.center());
    h.frame(Vec::new());
    h.frame(Vec::new());
    h.frame(vec![Event::Text("55".into())]);
    let seen = h.frame(vec![key(Key::Escape)]);
    assert!(applied(&seen).is_empty(), "{seen:?}");
    let seen = h.frame(vec![key(Key::Enter)]);
    assert!(applied(&seen).is_empty(), "entry closed: {seen:?}");
}

#[test]
fn escape_during_a_drag_cancels_and_ignores_the_rest() {
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let track = h.track("contrast");
    let mut seen = h.frame(vec![Event::PointerMoved(at(track, 0.5))]);
    seen.extend(h.frame(vec![button(at(track, 0.5), true)]));
    seen.extend(h.frame(vec![Event::PointerMoved(at(track, 0.6))]));
    seen.extend(h.frame(vec![key(Key::Escape)]));
    seen.extend(h.frame(vec![Event::PointerMoved(at(track, 0.9))]));
    seen.extend(h.frame(vec![button(at(track, 0.9), false)]));
    let intents = edits(&seen);
    let cancel = intents
        .iter()
        .position(|i| **i == EditIntent::CancelGesture)
        .expect("cancel");
    assert!(intents[cancel + 1..].is_empty(), "{intents:?}");
    assert!(!intents.contains(&&EditIntent::CommitGesture));
}

#[test]
fn a_clicked_slider_does_not_capture_arrow_keys() {
    // Arrows step through photos (library keys); a pointer click must not steal them.
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let track = h.track("contrast");
    h.click(at(track, 0.5));
    let seen = h.frame(vec![key(Key::ArrowRight)]);
    assert!(applied(&seen).is_empty(), "{seen:?}");
}

#[test]
fn nothing_is_emitted_for_any_control_when_the_document_is_not_ready() {
    let mut snap = snapshot(DevelopParams::default());
    snap.editor.document.status = DocumentStatus::Loading;
    let mut h = Harness::new(snap);
    let track = h.track("contrast");
    let mut seen = h.click(at(track, 0.8));
    seen.extend(h.drag(at(track, 0.3), &[at(track, 0.5)]));
    let value = h.value_rect("tint");
    seen.extend(h.click(value.center()));
    let reset = h.ui.develop.module_resets["light"];
    seen.extend(h.click(reset.center()));
    assert!(edits(&seen).is_empty(), "{seen:?}");
}

#[test]
fn colour_mixer_band_selection_drives_that_hsl_band() {
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let dot = h.ui.develop.band_dots[3];
    h.click(dot.center());
    h.frame(Vec::new());
    let track = h.track("hsl.3.h");
    let seen = h.click(at(track, 0.75));
    let all = applied(&seen);
    assert_eq!(all.len(), 1, "{seen:?}");
    assert!((all[0].hsl[3].h - 50.0).abs() < 3.0);
    assert_eq!(all[0].hsl[0].h, 0.0);
    assert_eq!(all[0].hsl[2].h, 0.0);
}

#[test]
fn clicking_empty_curve_area_adds_a_point() {
    let mut h = Harness::new(snapshot(DevelopParams::default()));
    let plot = h.ui.develop.curve_rect.expect("curve plot");
    let pos = Pos2::new(
        plot.left() + plot.width() * 0.3,
        plot.bottom() - plot.height() * 0.7,
    );
    let seen = h.click(pos);
    let all = applied(&seen);
    assert_eq!(all.len(), 1, "{seen:?}");
    let pts = &all[0].tone_curve.rgb;
    assert_eq!(pts.len(), 3);
    assert!(
        (pts[1].x - 0.3).abs() < 0.02 && (pts[1].y - 0.7).abs() < 0.02,
        "{pts:?}"
    );
    assert!(pts[0].x < pts[1].x && pts[1].x < pts[2].x);
}

#[test]
fn dragging_a_curve_point_is_a_gesture_and_double_click_removes_it() {
    let params = DevelopParams {
        tone_curve: core_model::ToneCurve {
            rgb: vec![
                core_model::CurvePoint { x: 0.0, y: 0.0 },
                core_model::CurvePoint { x: 0.5, y: 0.5 },
                core_model::CurvePoint { x: 1.0, y: 1.0 },
            ],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut h = Harness::new(snapshot(params));
    let plot = h.ui.develop.curve_rect.unwrap();
    let screen = |x: f32, y: f32| {
        Pos2::new(
            plot.left() + plot.width() * x,
            plot.bottom() - plot.height() * y,
        )
    };
    let seen = h.drag(screen(0.5, 0.5), &[screen(0.5, 0.6), screen(0.5, 0.8)]);
    let intents = edits(&seen);
    assert_eq!(
        intents.first(),
        Some(&&EditIntent::BeginGesture),
        "{intents:?}"
    );
    assert_eq!(intents.last(), Some(&&EditIntent::CommitGesture));
    let last = intents
        .iter()
        .rev()
        .find_map(|i| match i {
            EditIntent::UpdateGesture(p) => Some(&**p),
            _ => None,
        })
        .expect("update");
    assert_eq!(last.tone_curve.rgb.len(), 3);
    assert!((last.tone_curve.rgb[1].y - 0.8).abs() < 0.03);

    let mut seen = h.click(screen(0.5, 0.5));
    seen.extend(h.click(screen(0.5, 0.5)));
    let removed = applied(&seen).into_iter().last().expect("remove");
    assert_eq!(removed.tone_curve.rgb.len(), 2);
}

fn many_rows(n: i64) -> Vec<ImageRow> {
    (1..=n)
        .map(|id| ImageRow {
            id,
            filename: format!("img{id}.cr3"),
            ..Default::default()
        })
        .collect()
}

#[test]
fn filmstrip_only_asks_thumbs_for_visible_cells_and_click_selects() {
    let mut snap = snapshot(DevelopParams::default());
    snap.library.rows = Arc::new(many_rows(3000));
    snap.editor.selected = Some(ImageId(1500));
    snap.editor.image = Some(snap.library.rows[1499].clone());
    let mut h = Harness::new(snap);
    // Let the smooth scroll to the selection finish.
    for _ in 0..80 {
        h.frame(Vec::new());
    }
    h.thumbs.0 = 0;
    h.frame(Vec::new());
    let asked = h.thumbs.0;
    assert!(asked > 0 && asked < 60, "asked for {asked} thumbs");
    let cells = h.ui.develop.film_rects.clone();
    assert!(
        cells.iter().any(|(id, _)| *id == 1500),
        "selection scrolled into view"
    );
    let (id, rect) = cells[cells.len() / 2 - 2];
    assert_ne!(id, 1500);
    let seen = h.click(rect.center());
    assert!(
        seen.iter()
            .any(|c| matches!(c, Command::SelectImage(Some(sel)) if sel.0 == id)),
        "{seen:?}"
    );
}

#[test]
fn filmstrip_rating_stars_emit_set_cull_for_the_selection() {
    let mut snap = snapshot(DevelopParams::default());
    snap.library.rows = Arc::new(many_rows(5));
    snap.editor.selected = Some(ImageId(3));
    snap.editor.image = Some(snap.library.rows[2].clone());
    let mut h = Harness::new(snap);
    // Stars sit at the far left of the filmstrip, vertically centred in the 72 px panel.
    let y = 2600.0 - 36.0;
    let seen = h.click(Pos2::new(14.0 + 3.0 * 15.0 + 6.0, y));
    assert!(
        seen.iter().any(|c| matches!(
            c,
            Command::SetCull { image, change: CullChange::Rating(n) } if image.0 == 3 && *n > 0
        )),
        "{seen:?}"
    );
}
