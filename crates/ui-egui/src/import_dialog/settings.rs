//! Right-hand settings column: mode, destination, pairing and apply-during-import options.

use super::state::{CollectionChoice, ImportUi};
use crate::{theme, widgets};
use core_app::import::{ImportMode, ImportRequest, ImportView, Pairing};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::CullFlag;
use egui::{RichText, Ui};

const MODES: [(ImportMode, &str, &str); 3] = [
    (
        ImportMode::Copy,
        "Copy",
        "Copy into the library (routed by date); source untouched.",
    ),
    (
        ImportMode::Move,
        "Move",
        "Copy + verify, then Trash the originals.",
    ),
    (
        ImportMode::Reference,
        "Reference",
        "Catalog in place; source folder is watched.",
    ),
];

fn note(ui: &mut Ui, text: &str, color: egui::Color32) {
    ui.label(RichText::new(text).size(11.5).color(color));
}

pub(super) fn show(
    ui: &mut Ui,
    snapshot: &ApplicationSnapshot,
    view: &ImportView,
    state: &mut ImportUi,
    requests: &mut Vec<ImportRequest>,
) {
    theme::section_heading(ui, "Mode");
    let labels: Vec<&str> = MODES.iter().map(|m| m.1).collect();
    let current = MODES.iter().position(|m| m.0 == state.mode).unwrap_or(2);
    if let Some(i) = widgets::segmented(ui, &labels, current) {
        state.mode = MODES[i].0;
    }
    note(ui, MODES[current].2, theme::T2);
    if state.mode == ImportMode::Move {
        note(
            ui,
            "Warning: originals are sent to the Trash after a verified copy.",
            theme::STAR,
        );
    }

    if state.mode != ImportMode::Reference {
        theme::section_heading(ui, "Destination");
        match state.destination(view) {
            Some(dest) => {
                ui.label(theme::mono(dest.display().to_string()));
                note(
                    ui,
                    "Files are routed into YYYY/YYYY-MM-DD folders.",
                    theme::T3,
                );
            }
            None => note(
                ui,
                "No library root is configured. Choose a destination folder to import.",
                theme::REJECT,
            ),
        }
        if ui.button("Change…").clicked() {
            requests.push(ImportRequest::PickDestination);
        }
    }

    let (pairs, files) = state.selected_pairs(view);
    if pairs > 0 {
        theme::section_heading(ui, "RAW + JPEG pairs");
        note(ui, &format!("{pairs} pair(s) in the selection."), theme::T2);
        let choice = if state.pairing == Pairing::Pair { 0 } else { 1 };
        if let Some(i) = widgets::segmented(ui, &["Pair them", "Separate"], choice) {
            state.pairing = if i == 0 {
                Pairing::Pair
            } else {
                Pairing::Standalone
            };
        }
        match state.pairing {
            Pairing::Pair => note(
                ui,
                &format!("{pairs} photo(s): the JPEG/HEIF is linked to its RAW, both files kept."),
                theme::T3,
            ),
            Pairing::Standalone => note(
                ui,
                &format!("{files} files become separate photos."),
                theme::T3,
            ),
        }
    }

    theme::section_heading(ui, "Apply during import");
    if let Some(n) = widgets::star_rating(ui, state.rating, 14.0, true) {
        state.rating = n;
    }
    ui.horizontal(|ui| {
        for (label, flag) in [
            ("No flag", None),
            ("Pick", Some(CullFlag::Pick)),
            ("Reject", Some(CullFlag::Reject)),
        ] {
            if widgets::chip(ui, label, state.flag == flag).clicked() {
                state.flag = flag;
            }
        }
    });
    ui.add(
        egui::TextEdit::singleline(&mut state.keywords_text)
            .hint_text("keywords, comma, separated")
            .desired_width(f32::INFINITY),
    );
    collection_combo(ui, snapshot, state);
}

fn collection_combo(ui: &mut Ui, snapshot: &ApplicationSnapshot, state: &mut ImportUi) {
    let statics = snapshot.library.collections.iter().filter(|c| !c.is_smart);
    let text = match state.collection {
        CollectionChoice::None => "No collection".to_owned(),
        CollectionChoice::New => "New collection…".to_owned(),
        CollectionChoice::Existing(id) => snapshot
            .library
            .collections
            .iter()
            .find(|c| c.id == id)
            .map_or_else(|| "No collection".to_owned(), |c| c.name.clone()),
    };
    egui::ComboBox::from_id_salt("import_collection")
        .selected_text(text)
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(
                &mut state.collection,
                CollectionChoice::None,
                "No collection",
            );
            for c in statics {
                ui.selectable_value(
                    &mut state.collection,
                    CollectionChoice::Existing(c.id),
                    &c.name,
                );
            }
            ui.selectable_value(
                &mut state.collection,
                CollectionChoice::New,
                "New collection…",
            );
        });
    if state.collection == CollectionChoice::New {
        ui.add(
            egui::TextEdit::singleline(&mut state.new_collection_name)
                .hint_text("Collection name")
                .desired_width(f32::INFINITY),
        );
    }
}
