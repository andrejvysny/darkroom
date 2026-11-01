//! Develop panel: Exposure slider (gesture begin/update/commit/cancel), Undo/Redo, the separate
//! catalog-save and sidecar-mirror status lines, and the save-failure banner. Pure presentation:
//! it reads the snapshot and emits [`Command`]s; the only state it keeps ([`DevelopUi`]) is
//! transient input state (is a drag in flight), never the authoritative document.

use core_app::registry::{action, ActionId, EnabledContext};
use core_app::state::{ApplicationSnapshot, DocumentStatus, MirrorStatus};
use core_app::workflow::{Command, EditIntent};
use core_app::AppError;
use core_model::controls::control_spec;
use core_model::identity::ImageId;

/// Stable ids (egui keys persisted widget memory by these salts).
pub const DEVELOP_PANEL_ID: &str = "develop_panel";
pub const SAVE_ERROR_ID: &str = "develop_save_error";
pub const EXPOSURE_SLIDER_ID: &str = "develop_exposure";

/// Transient develop input state, owned by the host next to `PhotoUi`.
#[derive(Default)]
pub struct DevelopUi {
    /// A pointer drag on the Exposure slider is between `BeginGesture` and Commit/Cancel.
    gesture_active: bool,
    /// Esc cancelled the gesture; ignore slider changes until the pointer is released.
    cancelled: bool,
    /// The error the user dismissed; the banner stays hidden while the snapshot still holds it.
    dismissed: Option<AppError>,
    /// Screen rect of the Exposure slider rail from the last frame (test/automation hook).
    pub exposure_rect: Option<egui::Rect>,
}

/// Which actions the current snapshot enables (the registry decides; this only derives facts).
pub(crate) fn enabled_context(snapshot: &ApplicationSnapshot) -> EnabledContext {
    let document = &snapshot.editor.document;
    EnabledContext {
        running: snapshot.running,
        catalog_ready: snapshot.catalog_ready,
        has_selection: snapshot.editor.selected.is_some(),
        // A Ready document whose image row (hence save identity) has loaded.
        editable: document.status == DocumentStatus::Ready && snapshot.editor.image.is_some(),
        can_undo: document.can_undo,
        can_redo: document.can_redo,
    }
}

/// Cmd/Ctrl+Z and Cmd/Ctrl+Shift+Z. The more specific shortcut is consumed first: egui treats
/// extra Shift as matching a plain Cmd+Z.
pub(crate) fn shortcuts(ctx: &egui::Context, enabled: EnabledContext, commands: &mut Vec<Command>) {
    let redo = egui::KeyboardShortcut::new(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::Z,
    );
    let undo = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
    let (redo_hit, undo_hit) =
        ctx.input_mut(|i| (i.consume_shortcut(&redo), i.consume_shortcut(&undo)));
    if redo_hit && action(ActionId::Redo).enabled.is_enabled(enabled) {
        commands.push(Command::Edit(EditIntent::Redo));
    } else if undo_hit && action(ActionId::Undo).enabled.is_enabled(enabled) {
        commands.push(Command::Edit(EditIntent::Undo));
    }
}

/// Right-hand develop panel.
pub(crate) fn panel(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    develop: &mut DevelopUi,
    commands: &mut Vec<Command>,
) {
    let enabled = enabled_context(snapshot);
    egui::Panel::right(DEVELOP_PANEL_ID).show(root, |ui| {
        ui.heading("Develop");
        ui.horizontal(|ui| {
            for (id, intent) in [
                (ActionId::Undo, EditIntent::Undo),
                (ActionId::Redo, EditIntent::Redo),
            ] {
                let descriptor = action(id);
                if ui
                    .add_enabled(
                        descriptor.enabled.is_enabled(enabled),
                        egui::Button::new(descriptor.label),
                    )
                    .clicked()
                {
                    commands.push(Command::Edit(intent));
                }
            }
        });
        exposure(ui, snapshot, develop, enabled, commands);
        ui.separator();
        status_lines(ui, snapshot);
    });
}

fn exposure(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    develop: &mut DevelopUi,
    enabled: EnabledContext,
    commands: &mut Vec<Command>,
) {
    let spec = control_spec("exposure").expect("exposure control spec exists");
    let can_edit = action(ActionId::Exposure).enabled.is_enabled(enabled);
    let params = snapshot.editor.document.params.as_ref();
    let shown = params.map_or(spec.default as f32, |p| p.exposure);
    let mut value = shown;
    ui.label("Exposure");
    let response = ui
        .add_enabled_ui(can_edit, |ui| {
            ui.push_id(EXPOSURE_SLIDER_ID, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 160.0;
                    let response = ui.add(
                        egui::Slider::new(&mut value, spec.min as f32..=spec.max as f32)
                            .show_value(false),
                    );
                    ui.monospace(format!("{value:+.2} EV"));
                    response
                })
                .inner
            })
            .inner
        })
        .inner;
    develop.exposure_rect = Some(response.rect);
    if !can_edit {
        develop.gesture_active = false;
        develop.cancelled = false;
        return;
    }
    if response.drag_started() {
        develop.cancelled = false;
        develop.gesture_active = true;
        commands.push(Command::Edit(EditIntent::BeginGesture));
    }
    if develop.gesture_active && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        develop.gesture_active = false;
        develop.cancelled = true;
        commands.push(Command::Edit(EditIntent::CancelGesture));
    }
    if response.changed() && !develop.cancelled && spec.ui_to_stored(f64::from(value)).is_some() {
        match (develop.gesture_active, params) {
            (true, Some(params)) => {
                let mut next = (**params).clone();
                next.exposure = value;
                commands.push(Command::Edit(EditIntent::UpdateGesture(Box::new(next))));
            }
            (false, _) => commands.push(Command::Edit(EditIntent::SetExposure(value))),
            (true, None) => {}
        }
    }
    if response.drag_stopped() {
        if develop.gesture_active {
            commands.push(Command::Edit(EditIntent::CommitGesture));
        }
        develop.gesture_active = false;
        develop.cancelled = false;
    }
}

/// Catalog save and sidecar mirror are separate facts; the mirror is never worded as saved.
fn status_lines(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot) {
    let save = &snapshot.editor.save;
    let catalog = if save.saving {
        "Saving…".to_owned()
    } else if save.dirty {
        "Unsaved changes".to_owned()
    } else if let Some(revision) = save.saved_catalog_revision {
        format!("Saved to catalog (revision {})", revision.0)
    } else {
        "No unsaved changes".to_owned()
    };
    ui.label(catalog);
    if let Some(error) = &save.last_error {
        ui.colored_label(egui::Color32::RED, format!("Save failed: {error}"));
    }
    ui.label(mirror_line(&save.mirror));
}

pub(crate) fn mirror_line(mirror: &MirrorStatus) -> String {
    match mirror {
        MirrorStatus::Unknown => "Sidecar: unknown".to_owned(),
        MirrorStatus::Pending { owed_revision } => {
            format!("Sidecar: pending (revision {})", owed_revision.0)
        }
        MirrorStatus::Mirrored { revision } => {
            format!("Sidecar: mirrored (revision {})", revision.0)
        }
        MirrorStatus::Failing { error } => format!("Sidecar: failing — {error}"),
    }
}

/// The image a save error is about, when it names one: `SaveFailed` carries the filename of the
/// selected image or `image <id>` for another; `SourceChanged` concerns the selected image.
fn error_image(snapshot: &ApplicationSnapshot, error: &AppError) -> Option<(ImageId, String)> {
    let selected = snapshot.editor.image.as_ref();
    match error {
        AppError::SourceChanged => selected.map(|row| (ImageId(row.id), row.filename.clone())),
        AppError::SaveFailed { image, .. } => {
            if let Some(row) = selected.filter(|row| &row.filename == image) {
                return Some((ImageId(row.id), row.filename.clone()));
            }
            let id: i64 = image.strip_prefix("image ")?.parse().ok()?;
            Some((ImageId(id), image.clone()))
        }
        _ => None,
    }
}

pub(crate) fn is_save_error(error: &AppError) -> bool {
    matches!(error, AppError::SaveFailed { .. } | AppError::SourceChanged)
}

/// Banner for a failed save / changed source: the message, a Discard action when the error names
/// an image, and Dismiss. Editing continues underneath.
pub(crate) fn error_banner(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    develop: &mut DevelopUi,
    commands: &mut Vec<Command>,
) {
    let Some(error) = snapshot.error.as_ref().filter(|e| is_save_error(e)) else {
        develop.dismissed = None;
        return;
    };
    if develop.dismissed.as_ref() == Some(error) {
        return;
    }
    egui::Panel::top(SAVE_ERROR_ID).show(root, |ui| {
        ui.colored_label(egui::Color32::RED, error.to_string());
        ui.horizontal(|ui| {
            if let Some((id, name)) = error_image(snapshot, error) {
                if ui
                    .button(format!("Discard unsaved changes to {name}"))
                    .clicked()
                {
                    commands.push(Command::DiscardUnsavedEdits(id));
                }
            }
            if ui.button("Dismiss").clicked() {
                develop.dismissed = Some(error.clone());
            }
        });
    });
}
