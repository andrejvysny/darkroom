//! Native egui panels. Pure presentation: reads an immutable [`ApplicationSnapshot`], holds
//! transient UI input state ([`PhotoUi`]) and emits typed [`Command`]s. No GPU, no I/O, no
//! runtime — the host owns window, device/queue and platform integration.

mod develop;
mod panels;
mod photo;

pub use develop::{DevelopUi, DEVELOP_PANEL_ID, EXPOSURE_SLIDER_ID, SAVE_ERROR_ID};
pub use panels::{
    settings_notice_strip, CATALOG_ACTIONS_ID, LIBRARY_LIST_ID, METADATA_ID, SETTINGS_NOTICES_ID,
};
pub use photo::{cropped_frame_aspect, fit_output, viewport_revision, PhotoRequestPlan, PhotoUi};

use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use std::time::Instant;

/// The root UI. `command_error` is the last UI-command dispatch error (see
/// `State::last_command_error` in the host's `window.rs`); it is shown as a dismissible
/// red label in the status area and set to `None` when the user dismisses it.
/// `settings_notices` are the startup settings diagnostics (see [`settings_notice_strip`]).
pub fn root(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    reopen: &Command,
    command_error: &mut Option<(String, Instant)>,
    photo: &mut PhotoUi,
    settings_notices: &mut Vec<String>,
    develop: &mut DevelopUi,
) -> Vec<Command> {
    let mut commands = Vec::new();
    develop::shortcuts(
        root.ctx(),
        develop::enabled_context(snapshot),
        &mut commands,
    );
    panels::top_bar(root, snapshot, reopen, command_error, &mut commands);
    settings_notice_strip(root, settings_notices);
    develop::error_banner(root, snapshot, develop, &mut commands);
    develop::panel(root, snapshot, develop, &mut commands);
    panels::metadata_panel(root, snapshot);
    panels::library_list(root, snapshot, &mut commands);
    photo::central_panel(root, snapshot, photo);
    commands
}
