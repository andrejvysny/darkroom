//! "Delete rejected…" confirmation (two steps) and the result line shown afterwards. The dialog
//! only ever names the counts computed for exactly the query captured when it opened; the
//! destructive command carries that same query (ids are re-derived in the catalog).

use super::LibraryUi;
use crate::theme;
use core_app::state::ApplicationSnapshot;
use core_app::workflow::{Command, Mutation, MutationReport};
use core_model::catalog::QueryParams;
use egui::{Align2, RichText};

pub(crate) struct RejectDialog {
    query: Box<QueryParams>,
    requested: bool,
    confirm: bool,
}

impl RejectDialog {
    pub(crate) fn new(query: &QueryParams) -> Self {
        Self {
            query: Box::new(query.clone()),
            requested: false,
            confirm: false,
        }
    }
}

fn fmt_bytes(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).ceil())
    }
}

pub(crate) fn show(
    ctx: &egui::Context,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    result_line(ctx, snapshot, lib);
    let Some(dialog) = lib.reject_dialog.as_mut() else {
        return;
    };
    if !dialog.requested {
        dialog.requested = true;
        commands.push(Command::LoadRejectSummary(dialog.query.clone()));
    }
    let summary = snapshot
        .library
        .reject_summary
        .as_ref()
        .filter(|(query, _)| query == dialog.query.as_ref())
        .map(|(_, summary)| summary);
    let mut close = false;
    let mut rects = Vec::new();
    let response = egui::Modal::new(egui::Id::unique("library_reject_dialog")).show(ctx, |ui| {
        ui.set_width(340.0);
        crate::widgets::label_dialog(ui, "Delete rejected photos");
        ui.label(RichText::new("Delete rejected photos").strong().size(15.0));
        ui.add_space(6.0);
        let Some(summary) = summary else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Counting…");
            });
            if ui.button("Cancel").clicked() {
                close = true;
            }
            return;
        };
        if summary.images == 0 {
            ui.label("No rejected photos match the current filters.");
            if ui.button("Close").clicked() {
                close = true;
            }
            return;
        }
        let companions = if summary.companions > 0 {
            format!(" (+{} camera companions)", summary.companions)
        } else {
            String::new()
        };
        ui.label(format!(
            "{} photos{companions}, {} will move to the Trash.",
            summary.images,
            fmt_bytes(summary.bytes)
        ));
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                close = true;
            }
            if !dialog.confirm {
                let step = ui.button("Move to Trash…");
                rects.push(("trash_step1", step.rect));
                if step.clicked() {
                    dialog.confirm = true;
                }
            } else {
                let label = format!(
                    "Confirm — move {} files",
                    summary.images + summary.companions
                );
                let button = egui::Button::new(RichText::new(label).color(theme::REJECT));
                let confirm = ui.add(button);
                rects.push(("trash_confirm", confirm.rect));
                if confirm.clicked() {
                    commands.push(Command::Mutate(Mutation::DeleteRejected(
                        dialog.query.clone(),
                    )));
                    close = true;
                }
            }
        });
    });
    lib.button_rects.extend(rects);
    if close || response.should_close() {
        lib.reject_dialog = None;
    }
}

/// Dismissible line with the last delete-rejected outcome.
fn result_line(ctx: &egui::Context, snapshot: &ApplicationSnapshot, lib: &mut LibraryUi) {
    let revision = snapshot.library.tags_revision;
    let Some(MutationReport::RejectedDeleted(result)) = &snapshot.library.last_report else {
        return;
    };
    if revision == lib.report_dismissed {
        return;
    }
    let mut text = format!("Moved {} to the Trash", result.trashed);
    if result.companions > 0 {
        text.push_str(&format!(" (+{} companions)", result.companions));
    }
    if result.failed > 0 {
        text.push_str(&format!(", {} failed", result.failed));
    }
    egui::Area::new(egui::Id::unique("library_reject_result"))
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
