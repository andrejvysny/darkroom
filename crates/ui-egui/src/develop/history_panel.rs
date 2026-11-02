//! History tab: session undo/redo, revert/reset and the selected photo's named snapshots. Every
//! control only emits a command; the application owns the document and the snapshot list.

use super::presets_panel::list_row;
use super::{enabled_context, DevelopUi};
use crate::icons::{icon_button_enabled, Icon};
use crate::theme;
use crate::widgets::pill_button;
use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, EditIntent, Mutation, SettingsSource};
use egui::{FontId, RichText, Ui};

/// An open inline name field: a new snapshot (`target` `None`) or a rename of snapshot `target`.
struct Entry {
    target: Option<i64>,
    text: String,
    /// First frame: request focus.
    fresh: bool,
}

enum Entered {
    Pending,
    Cancelled,
    Submitted(String),
}

#[derive(Default)]
pub(super) struct HistoryUi {
    entry: Option<Entry>,
}

pub(super) fn show(
    ui: &mut Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    commands: &mut Vec<Command>,
) {
    let context = enabled_context(snapshot);
    let can_edit = action(ActionId::ApplySettings).enabled.is_enabled(context);
    // A name field belongs to one photo's list; losing the document closes it.
    if !can_edit {
        st.history.entry = None;
    }
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        for (id, label, intent) in [
            (ActionId::Undo, "Undo edit", EditIntent::Undo),
            (ActionId::Redo, "Redo edit", EditIntent::Redo),
        ] {
            if pill_button(ui, None, label, action(id).enabled.is_enabled(context)).clicked() {
                commands.push(Command::Edit(intent));
            }
        }
    });
    ui.add_space(4.0);
    if pill_button(ui, None, "Revert to opened state", can_edit).clicked() {
        commands.push(Command::Edit(EditIntent::RevertToOpened));
    }
    ui.add_space(4.0);
    if pill_button(ui, None, "Reset to default", can_edit).clicked() {
        commands.push(Command::Edit(EditIntent::ResetToDefault));
    }
    snapshots(ui, snapshot, st, can_edit, commands);
}

fn snapshots(
    ui: &mut Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    can_edit: bool,
    commands: &mut Vec<Command>,
) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("SNAPSHOTS")
                .size(10.5)
                .color(theme::T3)
                .extra_letter_spacing(1.0),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button_enabled(ui, Icon::Plus, "Create snapshot", can_edit).clicked() {
                st.history.entry = Some(Entry {
                    target: None,
                    text: String::new(),
                    fresh: true,
                });
            }
        });
    });
    if st
        .history
        .entry
        .as_ref()
        .is_some_and(|entry| entry.target.is_none())
    {
        match name_field(ui, &mut st.history.entry) {
            Entered::Submitted(text) => {
                if let (Some(image), Some(params)) = (
                    snapshot.editor.selected,
                    snapshot.editor.document.params.clone(),
                ) {
                    // Legacy parity: an empty name falls back to "Snapshot".
                    let name = if text.is_empty() {
                        "Snapshot".to_owned()
                    } else {
                        text
                    };
                    commands.push(Command::Mutate(Mutation::CreateSnapshot {
                        image,
                        name,
                        params,
                    }));
                }
            }
            Entered::Cancelled | Entered::Pending => {}
        }
    }
    for item in snapshot.editor.snapshots.iter() {
        if st
            .history
            .entry
            .as_ref()
            .is_some_and(|entry| entry.target == Some(item.id))
        {
            if let Entered::Submitted(text) = name_field(ui, &mut st.history.entry) {
                if !text.is_empty() {
                    commands.push(Command::Mutate(Mutation::RenameSnapshot {
                        id: item.id,
                        name: text,
                    }));
                }
            }
            continue;
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let icons_w = 2.0 * 26.0 + 2.0 * ui.spacing().item_spacing.x;
            let width = (ui.available_width() - icons_w).max(40.0);
            if list_row(ui, &item.name, can_edit, width).clicked() {
                // A snapshot is a complete look: no merge, no amount.
                commands.push(Command::ApplySettings {
                    source: SettingsSource::Snapshot(item.id),
                    amount: 1.0,
                    replace_all: true,
                });
            }
            let rename = format!("Rename {}", item.name);
            if icon_button_enabled(ui, Icon::Pencil, &rename, can_edit).clicked() {
                st.history.entry = Some(Entry {
                    target: Some(item.id),
                    text: item.name.clone(),
                    fresh: true,
                });
            }
            let delete = format!("Delete {}", item.name);
            if icon_button_enabled(ui, Icon::Trash, &delete, can_edit).clicked() {
                commands.push(Command::Mutate(Mutation::DeleteSnapshot(item.id)));
            }
        });
    }
}

/// Inline name field: Enter submits the trimmed text, Esc or losing focus cancels. The entry is
/// closed on submit and cancel.
fn name_field(ui: &mut Ui, entry: &mut Option<Entry>) -> Entered {
    let Some(open) = entry.as_mut() else {
        return Entered::Cancelled;
    };
    let output = ui.add(
        egui::TextEdit::singleline(&mut open.text)
            .hint_text("Snapshot name")
            .font(FontId::proportional(12.0))
            .desired_width(f32::INFINITY),
    );
    if open.fresh {
        output.request_focus();
        open.fresh = false;
        return Entered::Pending;
    }
    if !output.lost_focus() {
        return Entered::Pending;
    }
    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
    let text = open.text.trim().to_owned();
    *entry = None;
    if enter {
        Entered::Submitted(text)
    } else {
        Entered::Cancelled
    }
}
