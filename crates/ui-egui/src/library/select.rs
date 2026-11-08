//! Multi-selection: click / Cmd-click / Shift-range semantics, select-all, collapse and the
//! batch target set. The primary selection stays `snapshot.editor.selected` (what Develop opens).

use super::LibraryUi;
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use core_model::identity::ImageId;

/// Keeps the set consistent with the loaded rows and the primary selection. Call once per frame
/// before drawing.
pub(crate) fn sync(snapshot: &ApplicationSnapshot, lib: &mut LibraryUi) {
    let rows = &snapshot.library.rows;
    lib.selected
        .retain(|id| rows.iter().any(|row| row.id == *id));
    if lib.anchor.is_some_and(|a| !lib.selected.contains(&a)) {
        lib.anchor = None;
    }
    let primary = snapshot.editor.selected;
    if primary != lib.last_primary {
        lib.last_primary = primary;
        // A half-typed keyword belongs to the image it was typed for.
        lib.keyword_input.clear();
        // The primary moved somewhere the set does not cover (arrow keys, filmstrip, command):
        // the multi-selection no longer describes the user's intent.
        if let Some(id) = primary {
            if !lib.selected.contains(&id.0) {
                lib.selected.clear();
                lib.selected.insert(id.0);
                lib.anchor = Some(id.0);
            }
        }
    }
}

/// The images a batch/collection/keyword gesture applies to: the multi-set when more than one
/// image is selected, else the primary selection.
pub(crate) fn targets(snapshot: &ApplicationSnapshot, lib: &LibraryUi) -> Vec<ImageId> {
    if lib.selected.len() > 1 {
        lib.selected.iter().copied().map(ImageId).collect()
    } else {
        snapshot.editor.selected.into_iter().collect()
    }
}

/// A click on grid cell `id`. Plain: select only it. Command/Ctrl: toggle membership. Shift:
/// contiguous range in current row order from the anchor.
pub(crate) fn click(
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    id: i64,
    modifiers: egui::Modifiers,
    commands: &mut Vec<Command>,
) {
    let rows = &snapshot.library.rows;
    if modifiers.shift {
        let anchor = lib
            .anchor
            .or(snapshot.editor.selected.map(|p| p.0))
            .unwrap_or(id);
        let pos = |wanted: i64| rows.iter().position(|row| row.id == wanted);
        if let (Some(a), Some(b)) = (pos(anchor), pos(id)) {
            let (lo, hi) = (a.min(b), a.max(b));
            lib.selected = rows[lo..=hi].iter().map(|row| row.id).collect();
            lib.anchor = Some(anchor);
            select_primary(id, commands);
            return;
        }
    }
    if modifiers.command || modifiers.ctrl {
        if lib.selected.contains(&id) {
            if lib.selected.len() > 1 {
                lib.selected.remove(&id);
                if snapshot.editor.selected == Some(ImageId(id)) {
                    if let Some(next) = lib.selected.iter().next_back().copied() {
                        select_primary(next, commands);
                    }
                }
            }
        } else {
            lib.selected.insert(id);
            lib.anchor = Some(id);
            select_primary(id, commands);
        }
        return;
    }
    lib.selected.clear();
    lib.selected.insert(id);
    lib.anchor = Some(id);
    select_primary(id, commands);
}

/// `last_primary` deliberately tracks only what the snapshot reported, so a click that has not
/// round-tripped yet is not mistaken for an external selection change.
fn select_primary(id: i64, commands: &mut Vec<Command>) {
    commands.push(Command::SelectImage(Some(ImageId(id))));
}

/// Cmd/Ctrl+A: every loaded row; the primary is kept (or the first row becomes it).
pub(crate) fn select_all(
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    let rows = &snapshot.library.rows;
    if rows.is_empty() {
        return;
    }
    lib.selected = rows.iter().map(|row| row.id).collect();
    if snapshot.editor.selected.is_none() {
        lib.anchor = Some(rows[0].id);
        select_primary(rows[0].id, commands);
    }
}

/// Esc / "Done": back to just the primary selection.
pub(crate) fn collapse(snapshot: &ApplicationSnapshot, lib: &mut LibraryUi) {
    lib.selected.clear();
    if let Some(id) = snapshot.editor.selected {
        lib.selected.insert(id.0);
        lib.anchor = Some(id.0);
    }
}
