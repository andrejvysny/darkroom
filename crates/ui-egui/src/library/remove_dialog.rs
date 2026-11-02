//! "Remove from Library…": grid context menu, one-step confirmation and the result line. Only
//! catalog rows go; files and `.json` sidecars stay, so the dialog says so in plain words.

use super::LibraryUi;
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, Mutation, MutationReport};
use core_model::identity::ImageId;
use egui::{Align2, RichText};

pub(crate) struct RemoveDialog {
    images: Vec<ImageId>,
}

fn photos(n: usize) -> String {
    format!("{n} photo{}", if n == 1 { "" } else { "s" })
}

/// Right-clicking inside the multi-selection acts on all of it; anywhere else, on that row only
/// (the selection is left alone).
pub(crate) fn context_menu(
    response: &egui::Response,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    row_id: i64,
) {
    response.context_menu(|ui| {
        let item = ui.button("Remove from Library…");
        lib.button_rects.push(("remove_menu_item", item.rect));
        if item.clicked() {
            let mut group = lib.selected.clone();
            group.extend(snapshot.editor.selected.map(|p| p.0));
            let ids: Vec<i64> = if group.contains(&row_id) {
                group.into_iter().collect()
            } else {
                vec![row_id]
            };
            let images = ids.into_iter().map(ImageId).collect();
            lib.remove_dialog = Some(RemoveDialog { images });
            ui.close();
        }
    });
}

pub(crate) fn show(
    ctx: &egui::Context,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    result_line(ctx, snapshot, lib);
    let Some(dialog) = lib.remove_dialog.as_ref() else {
        return;
    };
    let title = format!("Remove {} from the library?", photos(dialog.images.len()));
    let mut close = false;
    let mut confirmed = false;
    let mut rects = Vec::new();
    let response = egui::Modal::new(egui::Id::unique("library_remove_dialog")).show(ctx, |ui| {
        ui.set_width(360.0);
        crate::widgets::label_dialog(ui, &title);
        ui.label(RichText::new(&title).strong().size(15.0));
        ui.add_space(6.0);
        ui.label(
            "The files stay on disk. Edits and ratings remain in their .json sidecars, so \
             re-importing restores them. Keywords and collection membership stored only in the \
             library are removed.",
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let cancel = ui.button("Cancel");
            rects.push(("remove_cancel", cancel.rect));
            close |= cancel.clicked();
            let remove = ui.button("Remove");
            rects.push(("remove_confirm", remove.rect));
            confirmed = remove.clicked();
        });
    });
    lib.button_rects.extend(rects);
    if confirmed {
        let images = dialog.images.clone();
        for image in &images {
            lib.selected.remove(&image.0);
        }
        commands.push(Command::Mutate(Mutation::RemoveFromLibrary { images }));
        close = true;
    }
    if close || response.should_close() {
        lib.remove_dialog = None;
    }
}

/// Dismissible line with the last remove-from-library outcome.
fn result_line(ctx: &egui::Context, snapshot: &ApplicationSnapshot, lib: &mut LibraryUi) {
    let revision = snapshot.library.tags_revision;
    let Some(MutationReport::RemovedFromLibrary(result)) = &snapshot.library.last_report else {
        return;
    };
    if revision == lib.report_dismissed {
        return;
    }
    let mut text = format!("Removed {} from the library", photos(result.removed));
    if result.companions > 0 {
        text.push_str(&format!(
            ", including {} paired file{}",
            result.companions,
            if result.companions == 1 { "" } else { "s" }
        ));
    }
    egui::Area::new(egui::Id::unique("library_remove_result"))
        .anchor(Align2::LEFT_BOTTOM, egui::vec2(12.0, -48.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(text);
                    if ui.small_button("×").on_hover_text("Dismiss").clicked() {
                        lib.report_dismissed = revision;
                    }
                });
            });
        });
}
