//! Library / Develop keyboard: mode switch, selection stepping and culling marks. Ignored while a
//! text field has focus and when Cmd/Ctrl/Alt is held (Cmd+Z lives in `develop::shortcuts`).

use super::{cull_target, mutate_enabled, select, selected_row, LibraryUi};
use crate::Mode;
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{ColorLabel, Command, CullChange, CullFlag, Mutation};
use core_model::identity::ImageId;
use egui::Key;

pub(crate) fn handle(
    ctx: &egui::Context,
    typing_at_start: bool,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    mode: &mut Mode,
    commands: &mut Vec<Command>,
) {
    if typing_at_start || ctx.text_edit_focused() || !matches!(*mode, Mode::Library | Mode::Develop)
    {
        return;
    }
    if *mode == Mode::Library && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::A))
    {
        select::select_all(snapshot, lib, commands);
        return;
    }
    let (pressed, held) = ctx.input(|i| {
        let m = i.modifiers;
        let pressed: Vec<Key> = i
            .events
            .iter()
            .filter_map(|event| match event {
                egui::Event::Key {
                    key, pressed: true, ..
                } => Some(*key),
                _ => None,
            })
            .collect();
        (pressed, m.command || m.ctrl || m.alt)
    });
    if held {
        return;
    }
    for key in pressed {
        match key {
            Key::G => *mode = Mode::Library,
            Key::Escape if *mode == Mode::Library && lib.reject_dialog.is_none() => {
                if lib.loupe {
                    lib.loupe = false;
                } else if lib.selected.len() > 1 {
                    select::collapse(snapshot, lib);
                }
            }
            Key::E | Key::Enter if *mode == Mode::Library && snapshot.editor.selected.is_some() => {
                *mode = Mode::Develop;
            }
            Key::D if snapshot.editor.selected.is_some() => *mode = Mode::Develop,
            Key::ArrowLeft | Key::OpenBracket => step(snapshot, -1, commands),
            Key::ArrowRight | Key::CloseBracket => step(snapshot, 1, commands),
            Key::ArrowUp if *mode == Mode::Library => {
                step(snapshot, -(lib.columns as isize), commands)
            }
            Key::ArrowDown if *mode == Mode::Library => {
                step(snapshot, lib.columns as isize, commands)
            }
            _ => cull(key, snapshot, lib, *mode, commands),
        }
    }
}

fn index_of_selected(snapshot: &ApplicationSnapshot) -> Option<usize> {
    let id = snapshot.editor.selected?;
    snapshot.library.rows.iter().position(|row| row.id == id.0)
}

/// Moves the selection by `delta` rows, clamped (no wrap), like the legacy `moveSelection`.
fn step(snapshot: &ApplicationSnapshot, delta: isize, commands: &mut Vec<Command>) {
    let Some(current) = index_of_selected(snapshot) else {
        return;
    };
    let last = snapshot.library.rows.len() - 1;
    let next = current.saturating_add_signed(delta).min(last);
    if next != current {
        commands.push(Command::SelectImage(Some(ImageId(
            snapshot.library.rows[next].id,
        ))));
    }
}

/// A culling key: the change, whether single-selection mode advances after it, and (for colour
/// keys) the label, whose toggle depends on the current marks.
enum Mark {
    Set(CullChange),
    Label(ColorLabel),
}

fn mark_for(key: Key) -> Option<Mark> {
    Some(match key {
        Key::Num0 => Mark::Set(CullChange::Rating(0)),
        Key::Num1 => Mark::Set(CullChange::Rating(1)),
        Key::Num2 => Mark::Set(CullChange::Rating(2)),
        Key::Num3 => Mark::Set(CullChange::Rating(3)),
        Key::Num4 => Mark::Set(CullChange::Rating(4)),
        Key::Num5 => Mark::Set(CullChange::Rating(5)),
        Key::P => Mark::Set(CullChange::Flag(CullFlag::Pick)),
        Key::X | Key::R => Mark::Set(CullChange::Flag(CullFlag::Reject)),
        Key::U => Mark::Set(CullChange::Flag(CullFlag::None)),
        Key::Num6 => Mark::Label(ColorLabel::Red),
        Key::Num7 => Mark::Label(ColorLabel::Yellow),
        Key::Num8 => Mark::Label(ColorLabel::Green),
        Key::Num9 => Mark::Label(ColorLabel::Blue),
        _ => return None,
    })
}

fn cull(
    key: Key,
    snapshot: &ApplicationSnapshot,
    lib: &LibraryUi,
    mode: Mode,
    commands: &mut Vec<Command>,
) {
    let Some(mark) = mark_for(key) else { return };
    if mode == Mode::Library && lib.selected.len() > 1 {
        batch_cull(mark, snapshot, lib, commands);
        return;
    }
    let Some(image) = cull_target(snapshot) else {
        return;
    };
    let Some(row) = selected_row(snapshot) else {
        return;
    };
    let (change, advances) = match mark {
        Mark::Set(change) => (change, true),
        Mark::Label(label) => (toggle_label(row.color_label.as_deref(), label), false),
    };
    commands.push(Command::SetCull { image, change });
    if advances && mode == Mode::Library {
        step(snapshot, 1, commands);
    }
}

/// One `Mutation::Cull` over every selected image; never advances the selection. A colour key
/// clears the label only when every selected image already carries it.
fn batch_cull(
    mark: Mark,
    snapshot: &ApplicationSnapshot,
    lib: &LibraryUi,
    commands: &mut Vec<Command>,
) {
    if !mutate_enabled(snapshot) {
        return;
    }
    let change = match mark {
        Mark::Set(change) => change,
        Mark::Label(label) => {
            let all_have = snapshot
                .library
                .rows
                .iter()
                .filter(|row| lib.selected.contains(&row.id))
                .all(|row| {
                    row.color_label.as_deref().and_then(ColorLabel::from_row) == Some(label)
                });
            CullChange::Label(if all_have { None } else { Some(label) })
        }
    };
    commands.push(Command::Mutate(Mutation::Cull {
        images: super::targets(snapshot, lib),
        change,
    }));
}

/// Pressing the label's key again clears it.
fn toggle_label(current: Option<&str>, label: ColorLabel) -> CullChange {
    let same = current.and_then(ColorLabel::from_row) == Some(label);
    CullChange::Label(if same { None } else { Some(label) })
}
