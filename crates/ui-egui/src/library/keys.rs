//! Library / Develop keyboard: mode switch, selection stepping and culling marks. Ignored while a
//! text field has focus and when Cmd/Ctrl/Alt is held (Cmd+Z lives in `develop::shortcuts`).

use super::{cull_target, selected_row, LibraryUi};
use crate::Mode;
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{ColorLabel, Command, CullChange, CullFlag};
use core_model::identity::ImageId;
use egui::Key;

pub(crate) fn handle(
    ctx: &egui::Context,
    snapshot: &ApplicationSnapshot,
    lib: &LibraryUi,
    mode: &mut Mode,
    commands: &mut Vec<Command>,
) {
    if ctx.text_edit_focused() || !matches!(*mode, Mode::Library | Mode::Develop) {
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
            Key::D if snapshot.editor.selected.is_some() => *mode = Mode::Develop,
            Key::ArrowLeft | Key::OpenBracket => step(snapshot, -1, commands),
            Key::ArrowRight | Key::CloseBracket => step(snapshot, 1, commands),
            Key::ArrowUp if *mode == Mode::Library => {
                step(snapshot, -(lib.columns as isize), commands)
            }
            Key::ArrowDown if *mode == Mode::Library => {
                step(snapshot, lib.columns as isize, commands)
            }
            _ => cull(key, snapshot, *mode, commands),
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

fn cull(key: Key, snapshot: &ApplicationSnapshot, mode: Mode, commands: &mut Vec<Command>) {
    let Some(image) = cull_target(snapshot) else {
        return;
    };
    let Some(row) = selected_row(snapshot) else {
        return;
    };
    let (change, advances) = match key {
        Key::Num0 => (CullChange::Rating(0), true),
        Key::Num1 => (CullChange::Rating(1), true),
        Key::Num2 => (CullChange::Rating(2), true),
        Key::Num3 => (CullChange::Rating(3), true),
        Key::Num4 => (CullChange::Rating(4), true),
        Key::Num5 => (CullChange::Rating(5), true),
        Key::P => (CullChange::Flag(CullFlag::Pick), true),
        Key::X | Key::R => (CullChange::Flag(CullFlag::Reject), true),
        Key::U => (CullChange::Flag(CullFlag::None), true),
        Key::Num6 => (
            toggle_label(row.color_label.as_deref(), ColorLabel::Red),
            false,
        ),
        Key::Num7 => (
            toggle_label(row.color_label.as_deref(), ColorLabel::Yellow),
            false,
        ),
        Key::Num8 => (
            toggle_label(row.color_label.as_deref(), ColorLabel::Green),
            false,
        ),
        Key::Num9 => (
            toggle_label(row.color_label.as_deref(), ColorLabel::Blue),
            false,
        ),
        _ => return,
    };
    commands.push(Command::SetCull { image, change });
    if advances && mode == Mode::Library {
        step(snapshot, 1, commands);
    }
}

/// Pressing the label's key again clears it.
fn toggle_label(current: Option<&str>, label: ColorLabel) -> CullChange {
    let same = current.and_then(ColorLabel::from_row) == Some(label);
    CullChange::Label(if same { None } else { Some(label) })
}
