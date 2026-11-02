//! Import dialog (modal window over Library): source folder, file list, mode/destination,
//! pairing, apply-during-import options, progress and the final report. Reads only the host's
//! [`ImportView`]; every action is an [`ImportRequest`].

mod list;
mod settings;
mod state;
mod toasts;

pub use state::ImportUi;

use crate::{icons::Icon, theme, widgets, ThumbSource};
use core_app::import::{ImportPhase, ImportRequest, ImportView};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use egui::{RichText, Ui};

pub const IMPORT_DIALOG_ID: &str = "import_dialog";

const LIST_W: f32 = 540.0;
const SIDE_W: f32 = 310.0;
const BODY_H: f32 = 430.0;

pub(crate) fn show(
    ctx: &egui::Context,
    snapshot: &ApplicationSnapshot,
    view: &ImportView,
    thumbs: &mut dyn ThumbSource,
    ui_state: &mut ImportUi,
    requests: &mut Vec<ImportRequest>,
    commands: &mut Vec<Command>,
) {
    ui_state.dialog_shown = view.open;
    ui_state.import_button = None;
    ui_state.import_enabled = false;
    if view.open {
        ui_state.sync(view);
        let modal = egui::Modal::new(egui::Id::unique(IMPORT_DIALOG_ID)).show(ctx, |ui| {
            body(ui, snapshot, view, thumbs, ui_state, requests);
        });
        if modal.should_close() {
            requests.push(ImportRequest::Close);
        }
    } else {
        ui_state.laid_out_rows = 0;
    }
    if !view.open {
        toasts::progress_pill(ctx, view, ui_state, requests);
    } else {
        ui_state.pill_rect = None;
    }
    toasts::result_toast(ctx, snapshot, view, requests, commands);
}

fn body(
    ui: &mut Ui,
    snapshot: &ApplicationSnapshot,
    view: &ImportView,
    thumbs: &mut dyn ThumbSource,
    state: &mut ImportUi,
    requests: &mut Vec<ImportRequest>,
) {
    ui.set_width(LIST_W + SIDE_W + 24.0);
    ui.label(RichText::new("Import photos").size(15.0).color(theme::T1));
    let busy = view.busy();
    ui.add_enabled_ui(!busy, |ui| source_row(ui, view, state, requests));
    ui.separator();
    if view.source.is_none() {
        empty_state(ui, requests);
    } else {
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(LIST_W, BODY_H),
                egui::Layout::top_down(egui::Align::Min),
                |ui| ui.add_enabled_ui(!busy, |ui| list_column(ui, view, state)),
            );
            ui.allocate_ui_with_layout(
                egui::vec2(SIDE_W, BODY_H),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    list::preview_box(ui, state, thumbs);
                    egui::ScrollArea::vertical()
                        .id_salt("import_settings")
                        .max_height(BODY_H - 180.0)
                        .show(ui, |ui| {
                            ui.add_enabled_ui(!busy, |ui| {
                                settings::show(ui, snapshot, view, state, requests);
                            });
                        });
                },
            );
        });
    }
    ui.separator();
    footer(ui, view, state, requests);
}

fn source_row(ui: &mut Ui, view: &ImportView, state: &ImportUi, requests: &mut Vec<ImportRequest>) {
    let _ = state;
    ui.horizontal(|ui| {
        match &view.source {
            Some(src) => {
                ui.label(theme::mono(src.display().to_string()));
            }
            None => {
                ui.label(RichText::new("No source selected").color(theme::T3));
            }
        }
        if view.source.is_some() && ui.button("Change…").clicked() {
            requests.push(ImportRequest::PickSource);
        }
        let mut recursive = view.recursive;
        if ui.checkbox(&mut recursive, "Include subfolders").changed() {
            requests.push(ImportRequest::SetRecursive(recursive));
        }
    });
}

fn empty_state(ui: &mut Ui, requests: &mut Vec<ImportRequest>) {
    ui.vertical_centered(|ui| {
        ui.add_space(BODY_H * 0.3);
        ui.label(RichText::new("Choose a card or folder to import from.").color(theme::T2));
        ui.add_space(8.0);
        if widgets::pill_button(ui, Some(Icon::Folder), "Choose a folder or card…", true).clicked()
        {
            requests.push(ImportRequest::PickSource);
        }
        ui.add_space(BODY_H * 0.3);
    });
}

fn list_column(ui: &mut Ui, view: &ImportView, state: &mut ImportUi) {
    if matches!(view.phase, ImportPhase::Listing) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new("Listing files…").color(theme::T2));
        });
        return;
    }
    toolbar(ui, view, state);
    let (done, total) = view.dedup;
    if done < total {
        ui.label(
            RichText::new(format!("Checking duplicates {done} / {total}"))
                .size(11.5)
                .color(theme::T3),
        );
    }
    list::show(ui, view, state, BODY_H - 70.0);
}

fn toolbar(ui: &mut Ui, view: &ImportView, state: &mut ImportUi) {
    ui.horizontal_wrapped(|ui| {
        if widgets::chip(ui, "Select all", false).clicked() {
            state.select_all(view);
        }
        if widgets::chip(ui, "None", false).clicked() {
            state.select_none(view);
        }
        if widgets::chip(ui, "Only new", false).clicked() {
            state.select_only_new(view);
        }
        ui.label(
            RichText::new(format!(
                "{} of {} selected",
                state.selected_count(),
                state.selectable_total()
            ))
            .color(theme::T2),
        );
    });
    let kinds: Vec<&'static str> = state.cache().map(|c| c.kinds.clone()).unwrap_or_default();
    ui.horizontal(|ui| {
        if widgets::chip(ui, "All", state.kind_filter.is_none()).clicked() {
            state.kind_filter = None;
        }
        for kind in kinds {
            let label = kind.to_uppercase();
            let on = state.kind_filter.as_deref() == Some(kind);
            if widgets::chip(ui, &label, on).clicked() {
                state.kind_filter = Some(kind.to_owned());
            }
        }
    });
    let mut skip = state.skip_duplicates;
    if ui
        .checkbox(&mut skip, "Skip duplicates (hash-verified)")
        .changed()
    {
        state.set_skip_duplicates(view, skip);
    }
}

fn footer(ui: &mut Ui, view: &ImportView, state: &mut ImportUi, requests: &mut Vec<ImportRequest>) {
    match view.phase {
        ImportPhase::Importing { done, total } | ImportPhase::Stopping { done, total } => {
            let stopping = matches!(view.phase, ImportPhase::Stopping { .. });
            ui.horizontal(|ui| {
                let label = if stopping {
                    "Stopping…".to_owned()
                } else {
                    format!("Importing {done} / {total}")
                };
                ui.label(RichText::new(label).color(theme::T1));
                ui.add(egui::ProgressBar::new(toasts::fraction(done, total)).desired_width(300.0));
                if ui
                    .add_enabled(!stopping, egui::Button::new("Stop"))
                    .clicked()
                {
                    requests.push(ImportRequest::Stop);
                }
                if ui.button("Hide").clicked() {
                    requests.push(ImportRequest::Close);
                }
            });
        }
        _ => {
            let blocker = state.import_blocker(view);
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    requests.push(ImportRequest::Close);
                }
                let label = format!("Import {} photos", state.selected_count());
                let button =
                    egui::Button::new(RichText::new(label).color(theme::STAGE)).fill(theme::ACCENT);
                let response = ui.add_enabled(blocker.is_none(), button);
                state.import_button = Some(response.rect);
                state.import_enabled = blocker.is_none();
                if response.clicked() {
                    if let Some(plan) = state.plan(view) {
                        requests.push(ImportRequest::Commit(Box::new(plan)));
                    }
                }
                if let Some(why) = blocker {
                    if view.source.is_some() {
                        ui.label(RichText::new(why).size(11.5).color(theme::T3));
                    }
                }
            });
        }
    }
}
