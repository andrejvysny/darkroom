//! Progress pill (dialog closed, import running) and the final-report / failure toast.

use super::state::ImportUi;
use crate::theme;
use core_app::import::{ImportPhase, ImportReport, ImportRequest, ImportView};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use egui::{Align2, Area, Frame, Id, Order, RichText};

fn card() -> Frame {
    Frame::new()
        .fill(theme::ELEV)
        .stroke(egui::Stroke::new(1.0, theme::LINE2))
        .corner_radius(theme::RADIUS_MD)
        .inner_margin(10)
}

pub(super) fn progress_pill(
    ctx: &egui::Context,
    view: &ImportView,
    state: &mut ImportUi,
    requests: &mut Vec<ImportRequest>,
) {
    state.pill_rect = None;
    let (done, total, stopping) = match view.phase {
        ImportPhase::Importing { done, total } => (done, total, false),
        ImportPhase::Stopping { done, total } => (done, total, true),
        _ => return,
    };
    let area = Area::new(Id::unique("import_pill"))
        .order(Order::Tooltip)
        .anchor(Align2::LEFT_BOTTOM, [14.0, -14.0])
        .show(ctx, |ui| {
            card().show(ui, |ui| {
                ui.horizontal(|ui| {
                    let label = if stopping {
                        "Stopping…".to_owned()
                    } else {
                        format!("Importing {done} / {total}")
                    };
                    ui.label(RichText::new(label).color(theme::T1));
                    ui.add(egui::ProgressBar::new(fraction(done, total)).desired_width(120.0));
                    if ui
                        .add_enabled(!stopping, egui::Button::new("Stop"))
                        .clicked()
                    {
                        requests.push(ImportRequest::Stop);
                    }
                });
            });
        });
    state.pill_rect = Some(area.response.rect);
}

pub(super) fn fraction(done: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

fn report_lines(report: &ImportReport) -> Vec<String> {
    let mut lines = vec![format!("Imported {}", report.added)];
    for (n, what) in [
        (report.skipped, "skipped"),
        (report.failed, "failed"),
        (report.paired, "paired"),
        (report.unsupported, "unsupported"),
        (report.source_retained, "originals kept (trash failed)"),
        (report.not_processed, "not processed"),
    ] {
        if n > 0 {
            lines.push(format!("{n} {what}"));
        }
    }
    lines
}

pub(super) fn result_toast(
    ctx: &egui::Context,
    snapshot: &ApplicationSnapshot,
    view: &ImportView,
    requests: &mut Vec<ImportRequest>,
    commands: &mut Vec<Command>,
) {
    let area = Area::new(Id::unique("import_result"))
        .order(Order::Tooltip)
        .anchor(Align2::LEFT_BOTTOM, [14.0, -14.0]);
    match &view.phase {
        ImportPhase::Finished(report) => {
            area.show(ctx, |ui| {
                card().show(ui, |ui| {
                    ui.label(RichText::new(report_lines(report).join(" · ")).color(theme::T1));
                    if report.interrupted {
                        ui.label(RichText::new("Stopped early").color(theme::STAR));
                    }
                    if let Some(err) = &report.options_error {
                        ui.label(
                            RichText::new(format!("Marks not applied: {err}")).color(theme::REJECT),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Show").clicked() {
                            commands.push(show_import(snapshot, report.session_id));
                            requests.push(ImportRequest::Dismiss);
                        }
                        if ui.button("Dismiss").clicked() {
                            requests.push(ImportRequest::Dismiss);
                        }
                    });
                });
            });
        }
        ImportPhase::Failed(message) => {
            area.show(ctx, |ui| {
                card().show(ui, |ui| {
                    ui.label(
                        RichText::new(format!("Import failed: {message}")).color(theme::REJECT),
                    );
                    if ui.button("Dismiss").clicked() {
                        requests.push(ImportRequest::Dismiss);
                    }
                });
            });
        }
        _ => {}
    }
}

/// The current library query narrowed to one import session, paging reset.
fn show_import(snapshot: &ApplicationSnapshot, session_id: i64) -> Command {
    let mut query = snapshot.library.query.clone();
    query.import_session_id = Some(session_id);
    query.offset = Some(0);
    query.cursor_value = None;
    query.cursor_id = None;
    Command::QueryLibrary(Box::new(query))
}
