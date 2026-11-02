//! Develop mode: left Presets/History rail, stage, instrument panel (histogram + modules),
//! filmstrip. Pure presentation: it reads the snapshot and emits [`Command`]s; the only state it
//! keeps ([`DevelopUi`]) is transient input state (drag in flight, open modules, selected band),
//! never the authoritative document.

mod color;
mod curve;
mod filmstrip;
mod left;
mod modules;
mod params;
mod slider;
pub(crate) mod stage;
pub(crate) mod zoom;

use crate::icons::{icon_button_enabled, Icon};
use crate::{theme, HistogramData, PhotoUi, ThumbSource};
use core_app::registry::{action, ActionId, EnabledContext};
use core_app::state::{ApplicationSnapshot, DocumentStatus, MirrorStatus};
use core_app::workflow::{Command, EditIntent};
use core_app::AppError;
use core_model::identity::ImageId;
use core_model::DevelopParams;
use egui::{Id, Margin, Rect};
use slider::Env;
use std::collections::HashMap;

/// Stable ids (egui keys persisted widget memory by these salts).
pub const DEVELOP_PANEL_ID: &str = "develop_panel";
pub const SAVE_ERROR_ID: &str = "develop_save_error";
pub const EXPOSURE_SLIDER_ID: &str = "develop_exposure";
const STATUS_RESERVE: f32 = 78.0;

/// An open numeric entry over a slider's value text.
pub(crate) struct Editing {
    id: Id,
    text: String,
    /// First frame: request focus and select the text.
    fresh: bool,
}

/// Transient develop input state, owned by the host next to `PhotoUi`.
#[derive(Default)]
pub struct DevelopUi {
    /// The widget whose pointer drag is between `BeginGesture` and Commit/Cancel (one at a time).
    gesture: Option<Id>,
    /// Esc cancelled the gesture; ignore changes until the pointer is released.
    cancelled: bool,
    /// Last value streamed in the active gesture (skip identical updates).
    last_sent: Option<f64>,
    editing: Option<Editing>,
    /// The error the user dismissed; the banner stays hidden while the snapshot still holds it.
    dismissed: Option<AppError>,
    /// Module open/closed overrides by module id.
    open: HashMap<&'static str, bool>,
    /// Colour-mixer band the Hue/Saturation/Luminance sliders edit.
    band: u8,
    /// Tone-curve channel tab (0 = RGB, 1 = R, 2 = G, 3 = B).
    curve_channel: usize,
    curve_grab: Option<curve::Grab>,
    left_collapsed: bool,
    left_tab: left::Tab,
    film_scrolled_to: Option<ImageId>,
    /// The keyboard was busy when the frame's input was read (a text edit owned it, or a modal
    /// is open; the root captures this before any widget runs); the stage's bare-`Z` zoom toggle
    /// must stay out of the way.
    typing: bool,
    /// Stage zoom/pan (fit by default; reset when the photo changes).
    zoom: zoom::ZoomUi,
    /// Screen rect of the Exposure slider track from the last frame (test/automation hook).
    pub exposure_rect: Option<Rect>,
    /// Track / value-text rect of every slider laid out last frame, by `ControlKey::name()`.
    pub slider_tracks: HashMap<String, Rect>,
    pub slider_values: HashMap<String, Rect>,
    /// "Reset" label rect of every module header by module id.
    pub module_resets: HashMap<&'static str, Rect>,
    /// Hue-band dots of the colour mixer, in band order.
    pub band_dots: Vec<Rect>,
    /// Tone-curve plot rect (the square the points live in).
    pub curve_rect: Option<Rect>,
    /// Screen rect of every filmstrip cell laid out last frame, `(image id, rect)`.
    pub film_rects: Vec<(i64, Rect)>,
}

impl DevelopUi {
    /// Record whether the keyboard was busy this frame (see [`crate::root`]'s capture), so the
    /// stage's bare-`Z` zoom toggle yields to a focused text edit or an open modal.
    pub(crate) fn set_typing(&mut self, typing: bool) {
        self.typing = typing;
    }

    /// Screen rect of the photo drawn on the stage last frame, in points (test/automation hook:
    /// the point a zoom gesture has to land on).
    pub fn stage_image_rect(&self) -> Option<Rect> {
        self.zoom.image_rect()
    }
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

/// Top-bar "Reset": every edit back to defaults (legacy `useDevelop.reset`: masks cleared too).
/// The intrinsic, non-persisted `display_referred` flag belongs to the source, not the edit.
pub(crate) fn reset_all_command(snapshot: &ApplicationSnapshot) -> Option<Command> {
    if !action(ActionId::Edit)
        .enabled
        .is_enabled(enabled_context(snapshot))
    {
        return None;
    }
    let current = snapshot.editor.document.params.as_ref()?;
    let next = DevelopParams {
        display_referred: current.display_referred,
        ..Default::default()
    };
    Some(Command::Edit(EditIntent::Apply(Box::new(next))))
}

/// All Develop panels around the stage.
pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    thumbs: &mut dyn ThumbSource,
    histogram: Option<&HistogramData>,
    photo: &mut PhotoUi,
    commands: &mut Vec<Command>,
) {
    filmstrip::show(root, snapshot, st, thumbs, commands);
    left::show(root, st);
    panel(root, snapshot, st, histogram, commands);
    stage::central(root, snapshot, photo, Some(&mut st.zoom), st.typing);
}

/// Right-hand instrument panel: undo/redo, histogram, scrolling modules, save status.
fn panel(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    st: &mut DevelopUi,
    histogram: Option<&HistogramData>,
    commands: &mut Vec<Command>,
) {
    let enabled = enabled_context(snapshot);
    st.slider_tracks.clear();
    st.slider_values.clear();
    st.module_resets.clear();
    st.band_dots.clear();
    st.curve_rect = None;
    let can_edit = action(ActionId::Edit).enabled.is_enabled(enabled);
    let selected = snapshot.editor.selected.map(|id| id.0);
    egui::Panel::right(DEVELOP_PANEL_ID)
        .resizable(true)
        .default_size(260.0)
        .size_range(240.0..=360.0)
        .frame(egui::Frame::new().fill(theme::APP))
        .show(root, |ui| {
            undo_redo_row(ui, enabled, commands);
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                egui::Frame::new()
                    .inner_margin(Margin::symmetric(14, 4))
                    .show(ui, |ui| {
                        crate::histogram::draw(ui, 110.0, histogram, selected.unwrap_or(-1));
                    });
            });
            let max_height = (ui.available_height() - STATUS_RESERVE).max(60.0);
            egui::ScrollArea::vertical()
                .id_salt(DEVELOP_PANEL_ID)
                .max_height(max_height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut env = Env {
                        params: snapshot.editor.document.params.as_ref(),
                        enabled: can_edit,
                        histogram: histogram.filter(|h| Some(h.image.0) == selected),
                        commands,
                    };
                    modules::all(ui, &mut env, st);
                });
            egui::Frame::new()
                .inner_margin(Margin::symmetric(14, 6))
                .show(ui, |ui| {
                    ui.separator();
                    status_lines(ui, snapshot);
                });
        });
}

fn undo_redo_row(ui: &mut egui::Ui, enabled: EnabledContext, commands: &mut Vec<Command>) {
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.spacing_mut().item_spacing.x = 2.0;
        for (id, icon, intent) in [
            (ActionId::Undo, Icon::Undo, EditIntent::Undo),
            (ActionId::Redo, Icon::Redo, EditIntent::Redo),
        ] {
            let descriptor = action(id);
            let on = descriptor.enabled.is_enabled(enabled);
            if icon_button_enabled(ui, icon, descriptor.label, on).clicked() {
                commands.push(Command::Edit(intent));
            }
        }
    });
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

#[cfg(test)]
mod tests {
    use super::*;
    use core_app::state::{
        DocumentView, EditorSnapshot, JobsSnapshot, LibrarySnapshot, SettingsSnapshot,
    };
    use core_model::catalog::ImageRow;
    use core_model::identity::{EditRevision, ProcessVersion};
    use std::sync::Arc;

    fn snapshot(status: DocumentStatus, params: DevelopParams) -> ApplicationSnapshot {
        ApplicationSnapshot {
            revision: 1,
            running: true,
            catalog_ready: true,
            catalog_epoch: Default::default(),
            library: LibrarySnapshot::default(),
            jobs: JobsSnapshot::default(),
            settings: SettingsSnapshot::default(),
            error: None,
            editor: EditorSnapshot {
                selected: Some(ImageId(1)),
                image: Some(ImageRow {
                    id: 1,
                    ..Default::default()
                }),
                document: DocumentView {
                    status,
                    params: Some(Arc::new(params)),
                    revision: Some(EditRevision(1)),
                    process_version: Some(ProcessVersion(6)),
                    ..Default::default()
                },
                ..Default::default()
            },
        }
    }

    #[test]
    fn reset_all_restores_defaults_but_keeps_the_source_flag() {
        let edited = DevelopParams {
            exposure: 2.0,
            display_referred: true,
            masks: vec![core_model::Mask::default()],
            ..Default::default()
        };
        let Some(Command::Edit(EditIntent::Apply(next))) =
            reset_all_command(&snapshot(DocumentStatus::Ready, edited))
        else {
            panic!("expected an Apply");
        };
        let expected = DevelopParams {
            display_referred: true,
            ..Default::default()
        };
        assert_eq!(*next, expected);
    }

    #[test]
    fn reset_all_is_unavailable_until_the_document_is_ready() {
        let loading = snapshot(DocumentStatus::Loading, DevelopParams::default());
        assert!(reset_all_command(&loading).is_none());
    }
}
