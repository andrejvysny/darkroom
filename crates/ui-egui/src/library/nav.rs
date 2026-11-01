//! Left catalog navigation: CATALOG / FOLDERS (capture-date tree) / file type filters. Sections
//! without native data are explicit placeholders.

use super::{requery, LibraryUi, NAV_ID, NOT_YET};
use crate::icons::Icon;
use crate::theme;
use crate::widgets::{nav_row, placeholder_row};
use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;

/// `(query value, label)`, as in the legacy `FILE_TYPES`.
const FILE_TYPES: [(&str, &str); 5] = [
    ("raw", "RAW"),
    ("heif", "HEIF"),
    ("hdr", "HDR"),
    ("jpeg", "JPEG"),
    ("png", "PNG"),
];

pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    request_dates(snapshot, lib, commands);
    egui::Panel::left(NAV_ID)
        .exact_size(200.0)
        .resizable(false)
        .frame(
            egui::Frame::new()
                .fill(theme::APP)
                .inner_margin(egui::Margin::symmetric(8, 4)),
        )
        .show(root, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("library_nav_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    catalog(ui, snapshot, commands);
                    folders(ui, snapshot, lib, commands);
                    for heading in ["Collections", "Smart", "Keywords"] {
                        theme::section_heading(ui, heading);
                        placeholder_row(ui, NOT_YET);
                    }
                    ui.add_space(8.0);
                    let scan = egui::Button::new("Run AI scan…")
                        .min_size(egui::vec2(ui.available_width(), 28.0));
                    ui.add_enabled(false, scan).on_disabled_hover_text(NOT_YET);
                    for heading in ["People", "Panoramas", "Detected"] {
                        theme::section_heading(ui, heading);
                        placeholder_row(ui, NOT_YET);
                    }
                    file_types(ui, snapshot, commands);
                });
        });
}

/// `LoadDates` once per catalog epoch, when the catalog is ready and the tree is still empty.
fn request_dates(snapshot: &ApplicationSnapshot, lib: &mut LibraryUi, commands: &mut Vec<Command>) {
    let enabled = action(ActionId::LoadDates)
        .enabled
        .is_enabled(crate::develop::enabled_context(snapshot));
    if enabled
        && snapshot.catalog_ready
        && snapshot.library.dates.is_empty()
        && lib.dates_requested != Some(snapshot.catalog_epoch)
    {
        lib.dates_requested = Some(snapshot.catalog_epoch);
        commands.push(Command::LoadDates);
    }
}

fn catalog(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot, commands: &mut Vec<Command>) {
    let query = &snapshot.library.query;
    let scoped = query.capture_year.is_some() || query.capture_date.is_some();
    let flag = query.flag.as_deref();
    let count = snapshot.library.count;
    theme::section_heading(ui, "Catalog");
    let rows = [
        ("All photos", Icon::Image, None, flag.is_none() && !scoped),
        (
            "Picks",
            Icon::Flag,
            Some("pick"),
            flag == Some("pick") && !scoped,
        ),
        (
            "Rejected",
            Icon::Trash,
            Some("reject"),
            flag == Some("reject") && !scoped,
        ),
    ];
    for (label, icon, filter, selected) in rows {
        let shown = selected.then_some(count);
        if nav_row(ui, Some(icon), label, shown, selected, 0.0).clicked() {
            commands.push(requery(snapshot, |q| {
                q.flag = filter.map(str::to_owned);
                q.capture_year = None;
                q.capture_date = None;
            }));
        }
    }
    placeholder_row(ui, "Recent import");
}

fn folders(
    ui: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    lib: &mut LibraryUi,
    commands: &mut Vec<Command>,
) {
    theme::section_heading(ui, "Folders");
    if snapshot.library.dates.is_empty() {
        placeholder_row(ui, "No dated photos");
        return;
    }
    let query = &snapshot.library.query;
    for year in snapshot.library.dates.iter() {
        let open = lib.expanded_years.contains(&year.year);
        let selected = query.capture_year.as_deref() == Some(year.year.as_str())
            && query.capture_date.is_none();
        let icon = if open {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        let response = nav_row(ui, Some(icon), &year.year, Some(year.count), selected, 0.0);
        if response.clicked() {
            let on_chevron = response
                .interact_pointer_pos()
                .is_some_and(|p| p.x < response.rect.left() + 28.0);
            if on_chevron {
                if open {
                    lib.expanded_years.remove(&year.year);
                } else {
                    lib.expanded_years.insert(year.year.clone());
                }
            } else if year.year != "Unknown" {
                lib.expanded_years.insert(year.year.clone());
                commands.push(requery(snapshot, |q| {
                    q.capture_year = Some(year.year.clone());
                    q.capture_date = None;
                }));
            }
        }
        if !open {
            continue;
        }
        for day in &year.dates {
            let selected = query.capture_date.as_deref() == Some(day.date.as_str());
            if nav_row(ui, None, &day.date, Some(day.count), selected, 18.0).clicked()
                && day.date != "Unknown"
            {
                commands.push(requery(snapshot, |q| {
                    q.capture_date = Some(day.date.clone());
                    q.capture_year = None;
                }));
            }
        }
    }
}

fn file_types(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot, commands: &mut Vec<Command>) {
    theme::section_heading(ui, "File type");
    let current = snapshot.library.query.format.as_deref();
    for (kind, label) in FILE_TYPES {
        let active = current == Some(kind);
        if nav_row(ui, Some(Icon::Image), label, None, active, 0.0).clicked() {
            commands.push(requery(snapshot, |q| {
                q.format = if active { None } else { Some(kind.to_owned()) };
            }));
        }
    }
}
