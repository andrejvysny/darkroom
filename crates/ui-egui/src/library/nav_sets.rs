//! Left nav: Collections, Smart collections and Keywords. Every change is a
//! `Command::Mutate`; selecting a row is a requery.

use super::filters::{
    apply_predicate, clear_predicate, has_predicate, predicate_active, predicate_of,
};
use super::{mutate_enabled, requery, targets, LibraryUi};
use crate::icons::Icon;
use crate::theme;
use crate::widgets::{nav_row, placeholder_row};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, Mutation};
use egui::Key;

enum Edit {
    Commit(String),
    Cancel,
}

/// Single-line editor that takes focus on its first frame; Enter commits, anything else that
/// drops focus (Esc, click away) cancels.
fn inline_edit(ui: &mut egui::Ui, id: egui::Id, text: &mut String, hint: &str) -> Option<Edit> {
    let response = ui.add(
        egui::TextEdit::singleline(text)
            .id(id)
            .hint_text(hint)
            .desired_width(ui.available_width()),
    );
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label(hint);
    });
    let first = ui.data_mut(|d| d.get_temp::<bool>(id).is_none());
    if first {
        response.request_focus();
        ui.data_mut(|d| d.insert_temp(id, true));
        return None;
    }
    if response.lost_focus() {
        ui.data_mut(|d| d.remove_temp::<bool>(id));
        let entered = ui.input(|i| i.key_pressed(Key::Enter));
        let name = text.trim().to_owned();
        return Some(if entered && !name.is_empty() {
            Edit::Commit(name)
        } else {
            Edit::Cancel
        });
    }
    None
}

pub(super) fn collections(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    theme::section_heading(ui, "Collections");
    let enabled = mutate_enabled(snapshot);
    let active = snapshot.library.query.collection_id;
    for c in snapshot.library.collections.iter().filter(|c| !c.is_smart) {
        if renaming(ui, lib, c.id, &c.name, commands) {
            continue;
        }
        let selected = active == Some(c.id);
        let response = nav_row(
            ui,
            Some(Icon::Layers),
            &c.name,
            Some(c.count),
            selected,
            0.0,
        );
        if response.clicked() {
            commands.push(requery(snapshot, |q| {
                q.collection_id = if selected { None } else { Some(c.id) };
            }));
        }
        row_menu(&response, lib, c.id, &c.name, enabled, commands);
    }
    if snapshot.library.collections.iter().all(|c| c.is_smart) && lib.new_collection.is_none() {
        placeholder_row(ui, "No collections yet");
    }
    if let Some(text) = lib.new_collection.as_mut() {
        let id = egui::Id::unique("library_new_collection");
        match inline_edit(ui, id, text, "Collection name") {
            Some(Edit::Commit(name)) => {
                lib.new_collection = None;
                commands.push(Command::Mutate(Mutation::CreateCollection {
                    name,
                    smart: None,
                    images: targets(snapshot, lib),
                }));
            }
            Some(Edit::Cancel) => lib.new_collection = None,
            None => {}
        }
    } else if enabled && nav_row(ui, Some(Icon::Plus), "New collection", None, false, 0.0).clicked()
    {
        lib.new_collection = Some(String::new());
    }
}

/// Draws the inline rename editor when `id` is being renamed; returns whether it did.
fn renaming(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    id: i64,
    _name: &str,
    commands: &mut Vec<Command>,
) -> bool {
    let Some((rename_id, text)) = lib.rename.as_mut() else {
        return false;
    };
    if *rename_id != id {
        return false;
    }
    let editor = egui::Id::unique(("library_rename", id));
    match inline_edit(ui, editor, text, "Name") {
        Some(Edit::Commit(name)) => {
            lib.rename = None;
            commands.push(Command::Mutate(Mutation::RenameCollection { id, name }));
        }
        Some(Edit::Cancel) => lib.rename = None,
        None => {}
    }
    true
}

fn row_menu(
    response: &egui::Response,
    lib: &mut LibraryUi,
    id: i64,
    name: &str,
    enabled: bool,
    commands: &mut Vec<Command>,
) {
    response.context_menu(|ui| {
        ui.add_enabled_ui(enabled, |ui| {
            if ui.button("Rename").clicked() {
                lib.rename = Some((id, name.to_owned()));
                ui.close();
            }
            if ui.button("Delete").clicked() {
                commands.push(Command::Mutate(Mutation::DeleteCollection(id)));
                ui.close();
            }
        });
    });
}

pub(super) fn smart(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    theme::section_heading(ui, "Smart");
    let enabled = mutate_enabled(snapshot);
    let query = &snapshot.library.query;
    for c in snapshot.library.collections.iter().filter(|c| c.is_smart) {
        if renaming(ui, lib, c.id, &c.name, commands) {
            continue;
        }
        let Some(predicate) = c.smart_query() else {
            placeholder_row(ui, &format!("{} (unreadable)", c.name));
            continue;
        };
        let active = predicate_active(query, &predicate);
        let response = nav_row(ui, Some(Icon::Sparkle), &c.name, Some(c.count), active, 0.0);
        if response.clicked() {
            commands.push(requery(snapshot, |q| {
                if active {
                    clear_predicate(q);
                } else {
                    apply_predicate(q, &predicate);
                }
            }));
        }
        row_menu(&response, lib, c.id, &c.name, enabled, commands);
    }
    if let Some(text) = lib.new_smart.as_mut() {
        let id = egui::Id::unique("library_new_smart");
        match inline_edit(ui, id, text, "Smart collection name") {
            Some(Edit::Commit(name)) => {
                lib.new_smart = None;
                commands.push(Command::Mutate(Mutation::CreateCollection {
                    name,
                    smart: Some(Box::new(predicate_of(query))),
                    images: Vec::new(),
                }));
            }
            Some(Edit::Cancel) => lib.new_smart = None,
            None => {}
        }
    } else if enabled && has_predicate(query) {
        let response = nav_row(
            ui,
            Some(Icon::Plus),
            "Save filters as smart…",
            None,
            false,
            0.0,
        );
        lib.button_rects.push(("save_smart", response.rect));
        if response.clicked() {
            lib.new_smart = Some(String::new());
        }
    }
}

pub(super) fn keywords(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    commands: &mut Vec<Command>,
) {
    theme::section_heading(ui, "Keywords");
    if snapshot.library.keywords.is_empty() {
        placeholder_row(ui, "No keywords yet");
        return;
    }
    let enabled = mutate_enabled(snapshot);
    let active = snapshot.library.query.keyword_id;
    for k in snapshot.library.keywords.iter() {
        let selected = active == Some(k.id);
        let response = nav_row(ui, Some(Icon::Tag), &k.name, Some(k.count), selected, 0.0);
        if response.clicked() {
            commands.push(requery(snapshot, |q| {
                q.keyword_id = if selected { None } else { Some(k.id) };
            }));
        }
        response.context_menu(|ui| {
            if ui
                .add_enabled(enabled, egui::Button::new("Delete keyword"))
                .clicked()
            {
                commands.push(Command::Mutate(Mutation::DeleteKeyword(k.id)));
                ui.close();
            }
        });
    }
}
