//! Settings notice strip.

/// Stable panel id (egui keys persisted panel memory by it).
pub const SETTINGS_NOTICES_ID: &str = "settings_notices";

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
