//! Floating batch island shown while more than one photo is selected. Every action is one
//! `Mutation` over the explicit selected ids.

use super::{mutate_enabled, select, targets, LibraryUi};
use crate::theme;
use crate::widgets::{chip, color_dots, star_rating};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, CullChange, CullFlag, Mutation};
use egui::{Align2, Key, RichText};

pub(crate) fn show(
    ctx: &egui::Context,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    if lib.selected.len() < 2 || lib.loupe || !mutate_enabled(snapshot) {
        return;
    }
    let images = targets(snapshot, lib);
    let mut done = false;
    egui::Area::new(egui::Id::unique("library_selection_bar"))
        .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -52.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} selected", images.len()))
                            .monospace()
                            .color(theme::T1),
                    );
                    let mut cull = |change: CullChange| {
                        commands.push(Command::Mutate(Mutation::Cull {
                            images: images.clone(),
                            change,
                        }));
                    };
                    if let Some(n) = star_rating(ui, 0, 14.0, true) {
                        cull(CullChange::Rating(n));
                    }
                    for (label, flag) in [
                        ("Pick", CullFlag::Pick),
                        ("Reject", CullFlag::Reject),
                        ("Unflag", CullFlag::None),
                    ] {
                        if chip(ui, label, false).clicked() {
                            cull(CullChange::Flag(flag));
                        }
                    }
                    if let Some(Some(label)) = color_dots(ui, None, 10.0) {
                        cull(CullChange::Label(Some(label)));
                    }
                    if chip(ui, "No label", false).clicked() {
                        cull(CullChange::Label(None));
                    }
                    keyword_field(ui, lib, &images, commands);
                    collection_combo(ui, snapshot, &images, commands);
                    if chip(ui, "Export…", false).clicked() {
                        lib.export_open = Some(images.clone());
                    }
                    if chip(ui, "Done", false).clicked() {
                        done = true;
                    }
                });
            });
        });
    if done {
        select::collapse(snapshot, lib);
    }
}

fn keyword_field(
    ui: &mut egui::Ui,
    lib: &mut LibraryUi,
    images: &[core_model::identity::ImageId],
    commands: &mut Vec<Command>,
) {
    let response = ui.add(
        egui::TextEdit::singleline(&mut lib.bar_keyword)
            .hint_text("Add keyword…")
            .desired_width(120.0),
    );
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_label("Add keyword to selection");
    });
    let entered = response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
    let name = lib.bar_keyword.trim().to_owned();
    if entered && !name.is_empty() {
        lib.bar_keyword.clear();
        commands.push(Command::Mutate(Mutation::AddKeyword {
            images: images.to_vec(),
            name,
        }));
    }
}

fn collection_combo(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    images: &[core_model::identity::ImageId],
    commands: &mut Vec<Command>,
) {
    let statics: Vec<_> = snapshot
        .library
        .collections
        .iter()
        .filter(|c| !c.is_smart)
        .collect();
    if statics.is_empty() {
        return;
    }
    egui::ComboBox::from_id_salt("selection_bar_collection")
        .selected_text("Add to collection…")
        .show_ui(ui, |ui| {
            for c in statics {
                if ui.selectable_label(false, &c.name).clicked() {
                    commands.push(Command::Mutate(Mutation::AddToCollection {
                        id: c.id,
                        images: images.to_vec(),
                    }));
                }
            }
        });
}
