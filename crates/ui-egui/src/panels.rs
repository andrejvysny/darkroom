//! Top bar, settings notice strip, library list and metadata panels.

use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use core_model::identity::ImageId;
use std::time::Instant;

/// Stable panel ids (egui derives widget ids from these salts; persisted memory such as scroll
/// offsets and panel sizes is keyed by them).
pub const CATALOG_ACTIONS_ID: &str = "catalog_actions";
pub const SETTINGS_NOTICES_ID: &str = "settings_notices";
pub const METADATA_ID: &str = "catalog_metadata";
pub const LIBRARY_LIST_ID: &str = "library_list";

/// Status-area strip for the settings diagnostics collected at startup
/// (`crate::settings::SettingsState::load`): a corrupt file repaired, a settings file written by
/// a newer build, legacy keys the bridge refused. Same red-label style as the command error, in
/// its own row under the top bar; the Dismiss button clears the whole strip. Renders nothing when
/// there is nothing to report, so the ordinary layout is unchanged.
pub fn settings_notice_strip(root: &mut egui::Ui, notices: &mut Vec<String>) {
    if notices.is_empty() {
        return;
    }
    egui::Panel::top(SETTINGS_NOTICES_ID).show(root, |ui| {
        // Vertical, not horizontal: a label in a horizontal layout never wraps, so a long
        // message (a quarantine path, a rejected legacy key) would push the rest of the strip —
        // including Dismiss — off the window.
        for notice in notices.iter() {
            ui.colored_label(egui::Color32::RED, notice);
        }
        if ui.small_button("Dismiss").clicked() {
            notices.clear();
        }
    });
}

/// Top bar: title, busy spinner, catalog actions, pagination and the dismissible command error.
pub(crate) fn top_bar(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    reopen: &Command,
    command_error: &mut Option<(String, Instant)>,
    commands: &mut Vec<Command>,
) {
    let context = crate::develop::enabled_context(snapshot);
    egui::Panel::top(CATALOG_ACTIONS_ID).show(root, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Darkroom");
            if snapshot.jobs.metrics.queued + snapshot.jobs.metrics.running > 0 {
                ui.spinner();
            }
            for (id, command) in [
                (ActionId::OpenCatalog, reopen.clone()),
                (ActionId::RefreshLibrary, Command::RefreshLibrary),
                (ActionId::CloseCatalog, Command::CloseCatalog),
            ] {
                let descriptor = action(id);
                if ui
                    .add_enabled(
                        descriptor.enabled.is_enabled(context),
                        egui::Button::new(descriptor.label),
                    )
                    .clicked()
                {
                    commands.push(command);
                }
            }
            pagination(ui, snapshot, commands);
            if let Some(message) = command_error.as_ref().map(|(message, _)| message.clone()) {
                ui.colored_label(egui::Color32::RED, &message);
                if ui.small_button("Dismiss").clicked() {
                    *command_error = None;
                }
            }
        });
    });
}

/// Library list (left): virtualised filename rows; a click selects the image.
pub(crate) fn library_list(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    commands: &mut Vec<Command>,
) {
    egui::Panel::left(LIBRARY_LIST_ID).show(root, |ui| {
        egui::ScrollArea::vertical().show_rows(
            ui,
            22.0,
            snapshot.library.rows.len(),
            |ui, range| {
                for index in range {
                    let row = &snapshot.library.rows[index];
                    if ui
                        .selectable_label(
                            snapshot.editor.selected == Some(ImageId(row.id)),
                            &row.filename,
                        )
                        .clicked()
                    {
                        commands.push(Command::SelectImage(Some(ImageId(row.id))));
                    }
                }
            },
        );
    });
}

/// Metadata panel (right) for the selected image.
pub(crate) fn metadata_panel(root: &mut egui::Ui, snapshot: &ApplicationSnapshot) {
    egui::Panel::right(METADATA_ID).show(root, |ui| metadata(ui, snapshot));
}

fn metadata(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot) {
    ui.heading("Photo information");
    if let Some(image) = &snapshot.editor.image {
        ui.label(&image.filename);
        ui.label(&image.path);
        for value in [
            &image.camera_make,
            &image.camera_model,
            &image.lens,
            &image.format,
        ]
        .into_iter()
        .flatten()
        {
            ui.label(value);
        }
        if let (Some(w), Some(h)) = (image.width, image.height) {
            ui.label(format!("{w} × {h}"));
        }
        if let Some(iso) = image.iso {
            ui.label(format!("ISO {iso}"));
        }
        if let Some(aperture) = image.aperture {
            ui.label(format!("f/{aperture}"));
        }
        if let Some(shutter) = &image.shutter {
            ui.label(shutter);
        }
        ui.label(format!("{} stars", image.stars));
    } else {
        ui.label("Select a photo");
    }
}

fn pagination(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot, commands: &mut Vec<Command>) {
    let query = &snapshot.library.query;
    let offset = query.offset.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(5000).max(1);
    let loaded = i64::try_from(snapshot.library.rows.len()).unwrap_or(i64::MAX);
    ui.label(format!(
        "{}–{} of {} photos",
        if loaded == 0 { 0 } else { offset + 1 },
        offset.saturating_add(loaded),
        snapshot.library.count
    ));
    for (label, next, enabled) in [
        ("Previous", offset.saturating_sub(limit).max(0), offset > 0),
        (
            "Next",
            offset.saturating_add(limit),
            offset.saturating_add(loaded) < snapshot.library.count,
        ),
    ] {
        if ui
            .add_enabled(snapshot.catalog_ready && enabled, egui::Button::new(label))
            .clicked()
        {
            let mut query = query.clone();
            query.offset = Some(next);
            query.limit = Some(limit);
            commands.push(Command::QueryLibrary(Box::new(query)));
        }
    }
}
