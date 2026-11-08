//! Export dialog (modal over Library/Develop): destination, filename template with presets,
//! format and quality, progress with cancel, and the result line. Reads only the host's
//! [`ExportView`]; every action is an [`ExportRequest`].

use crate::{theme, widgets, Mode};
use core_app::export::{
    plan_names, ExportFormat, ExportPhase, ExportRequest, ExportView, BUILTIN_PRESETS,
    DEFAULT_TEMPLATE,
};
use core_app::state::ApplicationSnapshot;
use core_model::identity::ImageId;
use egui::{RichText, Ui};

pub const EXPORT_DIALOG_ID: &str = "export_dialog";
/// Dialog name exposed to assistive technology.
pub const EXPORT_DIALOG_LABEL: &str = "Export photos";

/// Transient dialog input (text buffers) plus test/automation hooks.
#[derive(Default)]
pub struct ExportUi {
    template: String,
    /// The view's template as last copied into `template` (detects host-side changes).
    synced: Option<String>,
    preset_name: String,
    /// Screen rects of the key buttons laid out last frame: `export_start`, `export_choose`,
    /// `export_close`, `export_cancel`, `export_save_preset`, `export_delete_preset`.
    pub button_rects: Vec<(&'static str, egui::Rect)>,
    /// First target's resolved file name as shown in the preview line.
    pub preview: Option<String>,
    /// The quality slider was drawn last frame (JPEG only).
    pub quality_shown: bool,
    pub dialog_shown: bool,
}

/// Photos an Export gesture applies to right now: the batch selection in Library, the open photo
/// in Develop.
pub(crate) fn export_ids(
    snapshot: &ApplicationSnapshot,
    mode: Mode,
    library: &crate::LibraryUi,
) -> Vec<ImageId> {
    match mode {
        Mode::Library => crate::library::targets(snapshot, library),
        Mode::Develop => snapshot.editor.selected.into_iter().collect(),
        Mode::Duplicates => Vec::new(),
    }
}

pub(crate) fn show(
    ctx: &egui::Context,
    view: &ExportView,
    ui_state: &mut ExportUi,
    requests: &mut Vec<ExportRequest>,
) {
    ui_state.button_rects.clear();
    ui_state.preview = None;
    ui_state.quality_shown = false;
    ui_state.dialog_shown = view.open;
    if !view.open {
        ui_state.synced = None;
        return;
    }
    if ui_state.synced.as_deref() != Some(view.template.as_str()) {
        ui_state.template.clone_from(&view.template);
        ui_state.synced = Some(view.template.clone());
    }
    let modal = egui::Modal::new(egui::Id::unique(EXPORT_DIALOG_ID)).show(ctx, |ui| {
        body(ui, view, ui_state, requests);
    });
    if modal.should_close() && !view.busy() {
        requests.push(ExportRequest::Close);
    }
}

fn photos(n: usize) -> String {
    format!("{n} photo{}", if n == 1 { "" } else { "s" })
}

fn body(ui: &mut Ui, view: &ExportView, state: &mut ExportUi, requests: &mut Vec<ExportRequest>) {
    ui.set_width(520.0);
    widgets::label_dialog(ui, EXPORT_DIALOG_LABEL);
    ui.label(
        RichText::new(EXPORT_DIALOG_LABEL)
            .size(15.0)
            .color(theme::T1),
    );
    ui.label(RichText::new(photos(view.targets.len())).color(theme::T2));
    ui.add_space(6.0);
    let busy = view.busy();
    ui.add_enabled_ui(!busy, |ui| {
        destination_row(ui, view, state, requests);
        ui.add_space(6.0);
        presets_row(ui, view, state, requests);
        template_row(ui, view, state, requests);
        ui.add_space(6.0);
        format_row(ui, view, state, requests);
    });
    if let Some(error) = &view.error {
        ui.add_space(4.0);
        ui.label(RichText::new(error).color(theme::REJECT));
    }
    ui.add_space(8.0);
    status(ui, view);
    ui.separator();
    footer(ui, view, state, requests);
}

fn destination_row(
    ui: &mut Ui,
    view: &ExportView,
    state: &mut ExportUi,
    requests: &mut Vec<ExportRequest>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Destination").color(theme::T2));
        match &view.dir {
            Some(dir) => ui.label(theme::mono(dir.display().to_string())),
            None => ui.label(RichText::new("No folder chosen").color(theme::T3)),
        };
        let choose = ui.button("Choose…");
        state.button_rects.push(("export_choose", choose.rect));
        if choose.clicked() {
            requests.push(ExportRequest::PickDir);
        }
    });
}

fn presets_row(
    ui: &mut Ui,
    view: &ExportView,
    state: &mut ExportUi,
    requests: &mut Vec<ExportRequest>,
) {
    ui.horizontal_wrapped(|ui| {
        for (name, template) in BUILTIN_PRESETS {
            if widgets::chip(ui, name, view.template == template).clicked() {
                requests.push(ExportRequest::SetTemplate(template.to_owned()));
            }
        }
        for preset in &view.presets {
            if widgets::chip(ui, &preset.name, view.template == preset.template).clicked() {
                requests.push(ExportRequest::SetTemplate(preset.template.clone()));
            }
            let delete = ui
                .small_button("×")
                .on_hover_text(format!("Delete preset {}", preset.name));
            ui.ctx().accesskit_node_builder(delete.id, |node| {
                node.set_label(format!("Delete preset {}", preset.name));
            });
            state
                .button_rects
                .push(("export_delete_preset", delete.rect));
            if delete.clicked() {
                requests.push(ExportRequest::DeletePreset(preset.name.clone()));
            }
        }
    });
}

fn template_row(
    ui: &mut Ui,
    view: &ExportView,
    state: &mut ExportUi,
    requests: &mut Vec<ExportRequest>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Filename").color(theme::T2));
        let edit = ui.add(
            egui::TextEdit::singleline(&mut state.template)
                .hint_text(DEFAULT_TEMPLATE)
                .desired_width(300.0),
        );
        ui.ctx().accesskit_node_builder(edit.id, |node| {
            node.set_label("Filename template");
        });
        if edit.changed() {
            requests.push(ExportRequest::SetTemplate(state.template.clone()));
        }
    });
    ui.label(
        RichText::new("Tokens: {original} {date} {seq}")
            .size(11.5)
            .color(theme::T3),
    );
    // The preview follows what is typed, not the one-frame-old view.
    let names = plan_names(&view.targets, &state.template, view.format);
    if let Some(name) = names.into_iter().next() {
        ui.label(theme::mono(format!("→ {name}")));
        state.preview = Some(name);
    }
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut state.preset_name)
                .hint_text("Preset name")
                .desired_width(160.0),
        );
        ui.ctx().accesskit_node_builder(edit.id, |node| {
            node.set_label("Preset name");
        });
        let name = state.preset_name.trim().to_owned();
        let save = ui.add_enabled(!name.is_empty(), egui::Button::new("Save as preset"));
        state.button_rects.push(("export_save_preset", save.rect));
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (save.clicked() || entered) && !name.is_empty() {
            requests.push(ExportRequest::SetTemplate(state.template.clone()));
            requests.push(ExportRequest::SavePreset(name));
            state.preset_name.clear();
        }
    });
}

fn format_row(
    ui: &mut Ui,
    view: &ExportView,
    state: &mut ExportUi,
    requests: &mut Vec<ExportRequest>,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Format").color(theme::T2));
        let selected = usize::from(view.format == ExportFormat::Png);
        if let Some(index) = widgets::segmented(ui, &["JPEG", "PNG"], selected) {
            requests.push(ExportRequest::SetFormat(if index == 0 {
                ExportFormat::Jpeg
            } else {
                ExportFormat::Png
            }));
        }
        if view.format == ExportFormat::Jpeg {
            state.quality_shown = true;
            let mut quality = view.quality;
            ui.push_id("export_quality", |ui| {
                if ui
                    .add(egui::Slider::new(&mut quality, 1..=100).text("Quality"))
                    .changed()
                {
                    requests.push(ExportRequest::SetQuality(quality));
                }
            });
        }
    });
}

fn status(ui: &mut Ui, view: &ExportView) {
    match &view.phase {
        ExportPhase::Idle => {}
        ExportPhase::Running {
            done,
            failed,
            total,
        } => progress(
            ui,
            format!("Exporting {} / {total}…", done + failed),
            done + failed,
            *total,
        ),
        ExportPhase::Cancelling {
            done,
            failed,
            total,
        } => progress(ui, "Cancelling…".to_owned(), done + failed, *total),
        ExportPhase::Finished {
            done,
            failed,
            first_error,
            dir,
        } => {
            let color = if *failed > 0 {
                theme::REJECT
            } else {
                theme::T1
            };
            ui.label(
                RichText::new(summary(view, *done, *failed, first_error.as_deref(), dir))
                    .color(color),
            );
        }
    }
}

fn progress(ui: &mut Ui, label: String, finished: usize, total: usize) {
    let fraction = if total == 0 {
        0.0
    } else {
        finished as f32 / total as f32
    };
    ui.add(
        egui::ProgressBar::new(fraction)
            .desired_width(480.0)
            .text(label),
    );
}

/// Legacy result wording: "Exported N → dir" / "Exported N, F failed → dir: firstError".
fn summary(
    view: &ExportView,
    done: usize,
    failed: usize,
    first_error: Option<&str>,
    dir: &std::path::Path,
) -> String {
    let where_to = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut text = if failed > 0 {
        format!("Exported {done}, {failed} failed → {where_to}")
    } else {
        format!("Exported {done} → {where_to}")
    };
    if let Some(error) = first_error {
        text.push_str(&format!(": {error}"));
    }
    let skipped = view.targets.len().saturating_sub(done + failed);
    if skipped > 0 {
        text.push_str(&format!(" (cancelled, {skipped} not exported)"));
    }
    text
}

fn footer(ui: &mut Ui, view: &ExportView, state: &mut ExportUi, requests: &mut Vec<ExportRequest>) {
    ui.horizontal(|ui| match view.phase {
        ExportPhase::Running { .. } | ExportPhase::Cancelling { .. } => {
            let cancelling = matches!(view.phase, ExportPhase::Cancelling { .. });
            let label = if cancelling {
                "Cancelling…"
            } else {
                "Cancel export"
            };
            let cancel = ui.add_enabled(!cancelling, egui::Button::new(label));
            state.button_rects.push(("export_cancel", cancel.rect));
            if cancel.clicked() {
                requests.push(ExportRequest::Cancel);
            }
        }
        _ => {
            let finished = matches!(view.phase, ExportPhase::Finished { .. });
            let close = ui.button(if finished { "Close" } else { "Cancel" });
            state.button_rects.push(("export_close", close.rect));
            if close.clicked() {
                requests.push(ExportRequest::Close);
            }
            let ready = view.dir.is_some() && !state.template.trim().is_empty();
            let label = if finished {
                "Export again".to_owned()
            } else {
                format!("Export {}", photos(view.targets.len()))
            };
            let start = ui.add_enabled(
                ready,
                egui::Button::new(RichText::new(label).color(theme::STAGE)).fill(theme::ACCENT),
            );
            state.button_rects.push(("export_start", start.rect));
            if start.clicked() {
                requests.push(ExportRequest::Start);
            }
            if !ready {
                let why = if view.dir.is_none() {
                    "Choose a destination folder"
                } else {
                    "Enter a filename template"
                };
                ui.label(RichText::new(why).size(11.5).color(theme::T3));
            }
        }
    });
}
