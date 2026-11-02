//! Info-panel sections backed by the image's tags: Keywords, Collections, Paired files. Tags are
//! rendered only when they belong to the shown image.

use super::{mutate_enabled, LibraryUi};
use crate::theme;
use crate::widgets::{chip, placeholder_row};
use core_app::registry::{action, ActionId};
use core_app::state::{ApplicationSnapshot, ImageTags};
use core_app::workflow::{Command, Mutation};
use core_model::identity::ImageId;
use egui::{Key, RichText};

/// Emits `LoadImageTags(primary)` once per `(image, tags_revision)`.
pub(super) fn request(
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    let Some(primary) = snapshot.editor.selected else {
        return;
    };
    let enabled = action(ActionId::LoadImageTags)
        .enabled
        .is_enabled(crate::develop::enabled_context(snapshot));
    let revision = snapshot.library.tags_revision;
    if enabled && lib.tags_requested != Some((primary, revision)) {
        lib.tags_requested = Some((primary, revision));
        commands.push(Command::LoadImageTags(primary));
    }
}

pub(super) fn show(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    primary: ImageId,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    let tags = snapshot
        .library
        .tags
        .as_ref()
        .filter(|t| t.image == primary);
    let enabled = mutate_enabled(snapshot);
    theme::section_heading(ui, "Keywords");
    let Some(tags) = tags else {
        placeholder_row(ui, "Loading…");
        return;
    };
    keywords(ui, tags, lib, enabled, commands);
    theme::section_heading(ui, "Collections");
    collections(ui, snapshot, tags, enabled, commands);
    pairs(ui, snapshot, tags, enabled, commands);
}

fn keywords(
    ui: &mut egui::Ui,
    tags: &ImageTags,
    lib: &mut LibraryUi,
    enabled: bool,
    commands: &mut Vec<Command>,
) {
    ui.horizontal_wrapped(|ui| {
        for k in &tags.keywords {
            let response = chip(ui, &format!("{} ×", k.name), false)
                .on_hover_text("Remove keyword from this photo");
            if enabled && response.clicked() {
                commands.push(Command::Mutate(Mutation::RemoveKeyword {
                    images: vec![tags.image],
                    keyword_id: k.id,
                }));
            }
        }
    });
    if tags.keywords.is_empty() {
        placeholder_row(ui, "No keywords");
    }
    let response = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(&mut lib.keyword_input)
            .hint_text("Add keyword…")
            .desired_width(ui.available_width()),
    );
    let entered = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    let name = lib.keyword_input.trim().to_owned();
    if entered && !name.is_empty() {
        lib.keyword_input.clear();
        commands.push(Command::Mutate(Mutation::AddKeyword {
            images: vec![tags.image],
            name,
        }));
        response.request_focus();
    }
}

fn collections(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    tags: &ImageTags,
    enabled: bool,
    commands: &mut Vec<Command>,
) {
    ui.horizontal_wrapped(|ui| {
        for c in tags.collections.iter().filter(|c| !c.is_smart) {
            let response = chip(ui, &format!("{} ×", c.name), false)
                .on_hover_text("Remove this photo from the collection");
            if enabled && response.clicked() {
                commands.push(Command::Mutate(Mutation::RemoveFromCollection {
                    id: c.id,
                    images: vec![tags.image],
                }));
            }
        }
    });
    let addable: Vec<_> = snapshot
        .library
        .collections
        .iter()
        .filter(|c| !c.is_smart && !tags.collections.iter().any(|m| m.id == c.id))
        .collect();
    if addable.is_empty() {
        return;
    }
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt("info_add_collection")
            .selected_text("Add to collection…")
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for c in addable {
                    if ui.selectable_label(false, &c.name).clicked() {
                        commands.push(Command::Mutate(Mutation::AddToCollection {
                            id: c.id,
                            images: vec![tags.image],
                        }));
                    }
                }
            });
    });
}

fn pairs(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    tags: &ImageTags,
    enabled: bool,
    commands: &mut Vec<Command>,
) {
    let Some(pair) = &tags.pair else { return };
    theme::section_heading(ui, "Paired files");
    let primary_name = snapshot
        .library
        .rows
        .iter()
        .find(|r| r.id == pair.primary_id)
        .map_or_else(|| format!("#{}", pair.primary_id), |r| r.filename.clone());
    ui.label(
        RichText::new(format!("RAW  {primary_name}"))
            .monospace()
            .size(11.0)
            .color(theme::T2),
    );
    for s in &pair.secondaries {
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(&s.filename)
                        .monospace()
                        .size(11.0)
                        .color(theme::T2),
                )
                .truncate(),
            )
            .on_hover_text(&s.path);
            if ui
                .add_enabled(enabled, egui::Button::new("Unpair").small())
                .on_hover_text("Drops the link only; no file is touched")
                .clicked()
            {
                commands.push(Command::Mutate(Mutation::UnlinkPair {
                    secondary: ImageId(s.id),
                }));
            }
        });
    }
    let note = if pair.role == "primary" {
        "Hidden from the grid; edits and metadata stay per file."
    } else {
        "This file is a companion of its RAW."
    };
    ui.label(RichText::new(note).size(10.5).color(theme::T3));
}
