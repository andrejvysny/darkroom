//! Library mode: catalog nav (left), virtualised thumbnail grid (centre), info panel (right),
//! toolbar (bottom) and the culling keyboard. Everything derives from the snapshot; every change
//! is a [`Command`].

mod filters;
mod grid;
mod info;
mod info_tags;
mod keys;
mod nav;
mod nav_sets;
mod reject_dialog;
mod select;
mod selection_bar;
mod toolbar;

use crate::{HistogramData, PhotoUi, ThumbSource};
use core_app::import::ImportRequest;
use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use core_model::catalog::{ImageRow, QueryParams};
use core_model::identity::{CatalogEpoch, ImageId};
use std::collections::{BTreeSet, HashSet};

pub(crate) use keys::handle as handle_keys;
pub(crate) use select::targets;

pub const NAV_ID: &str = "library_nav";
pub const INFO_ID: &str = "library_info";
pub const TOOLBAR_ID: &str = "library_toolbar";
pub const GRID_SCROLL_ID: &str = "library_grid_scroll";
pub const DEFAULT_CELL: f32 = 165.0;
pub const NOT_YET: &str = "Not in the native app yet";

/// Transient Library input state (never authoritative catalog state).
pub struct LibraryUi {
    /// Grid cell edge in points (zoom slider, 100..=320).
    pub cell_size: f32,
    /// Search box text.
    pub search: String,
    /// Time (egui seconds) of the last un-sent search edit, for the 300 ms debounce.
    pub(crate) search_edited_at: Option<f64>,
    /// Catalog epoch for which `LoadDates` was already dispatched.
    pub(crate) dates_requested: Option<CatalogEpoch>,
    pub(crate) expanded_years: HashSet<String>,
    /// Selection the grid last scrolled to.
    pub(crate) scrolled_to: Option<ImageId>,
    /// Columns laid out in the last grid frame (keyboard Up/Down step).
    pub(crate) columns: usize,
    /// Screen rect of every cell laid out in the last frame, `(image id, rect)` — test and
    /// automation hook; shows exactly which rows were virtualised in.
    pub cell_rects: Vec<(i64, egui::Rect)>,
    /// Multi-selection (image ids). The primary (`snapshot.editor.selected`) is always a member
    /// once the snapshot has caught up with the click.
    /// Screen rects of key buttons laid out in the last frame, by name (test and automation
    /// hook, like `cell_rects`): `delete_rejected`, `trash_step1`, `trash_confirm`, `save_smart`,
    /// `add_photos`.
    pub button_rects: Vec<(&'static str, egui::Rect)>,
    pub selected: BTreeSet<i64>,
    /// Shift-range anchor.
    pub(crate) anchor: Option<i64>,
    /// Primary seen last frame; a change to an id outside `selected` collapses the set.
    pub(crate) last_primary: Option<ImageId>,
    /// Loupe (single large image) instead of the grid.
    pub loupe: bool,
    /// `(image, tags_revision)` for which `LoadImageTags` was already dispatched.
    pub(crate) tags_requested: Option<(ImageId, u64)>,
    /// Info-panel / selection-bar text fields.
    pub(crate) keyword_input: String,
    pub(crate) bar_keyword: String,
    /// Inline nav editors: new static collection, new smart collection, rename `(id, text)`.
    pub(crate) new_collection: Option<String>,
    pub(crate) new_smart: Option<String>,
    pub(crate) rename: Option<(i64, String)>,
    pub(crate) reject_dialog: Option<reject_dialog::RejectDialog>,
    /// `tags_revision` whose delete-rejected result line was dismissed.
    pub(crate) report_dismissed: u64,
}

impl LibraryUi {
    /// A Library modal (delete-rejected confirmation) is open; global keys are suspended.
    pub(crate) fn modal_open(&self) -> bool {
        self.reject_dialog.is_some()
    }
}

impl Default for LibraryUi {
    fn default() -> Self {
        Self {
            cell_size: DEFAULT_CELL,
            search: String::new(),
            search_edited_at: None,
            dates_requested: None,
            expanded_years: HashSet::new(),
            scrolled_to: None,
            columns: 1,
            cell_rects: Vec::new(),
            button_rects: Vec::new(),
            selected: BTreeSet::new(),
            anchor: None,
            last_primary: None,
            loupe: false,
            tags_requested: None,
            keyword_input: String::new(),
            bar_keyword: String::new(),
            new_collection: None,
            new_smart: None,
            rename: None,
            reject_dialog: None,
            report_dismissed: 0,
        }
    }
}

/// A query command: the current query with `change` applied, paging reset, sort/search/limit kept.
pub(crate) fn requery(
    snapshot: &ApplicationSnapshot,
    change: impl FnOnce(&mut QueryParams),
) -> Command {
    let mut query = snapshot.library.query.clone();
    change(&mut query);
    query.offset = Some(0);
    query.cursor_value = None;
    query.cursor_id = None;
    Command::QueryLibrary(Box::new(query))
}

/// The selected image's row (library page first, else the editor's copy).
pub(crate) fn selected_row(snapshot: &ApplicationSnapshot) -> Option<&ImageRow> {
    let id = snapshot.editor.selected?;
    snapshot
        .library
        .rows
        .iter()
        .find(|row| row.id == id.0)
        .or_else(|| snapshot.editor.image.as_ref().filter(|row| row.id == id.0))
}

/// Whether the registry allows culling marks right now.
pub(crate) fn cull_enabled(snapshot: &ApplicationSnapshot) -> bool {
    action(ActionId::SetCull)
        .enabled
        .is_enabled(crate::develop::enabled_context(snapshot))
}

/// The image culling marks apply to: the selection, when `SetCull` is enabled.
pub(crate) fn cull_target(snapshot: &ApplicationSnapshot) -> Option<ImageId> {
    selected_row(snapshot)
        .filter(|_| cull_enabled(snapshot))
        .map(|row| ImageId(row.id))
}

/// Whether catalog mutations are allowed right now (registry-driven).
pub(crate) fn mutate_enabled(snapshot: &ApplicationSnapshot) -> bool {
    action(ActionId::Mutate)
        .enabled
        .is_enabled(crate::develop::enabled_context(snapshot))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    thumbs: &mut dyn ThumbSource,
    histogram: Option<&HistogramData>,
    lib: &mut LibraryUi,
    photo: &mut PhotoUi,
    import_requests: &mut Vec<ImportRequest>,
    commands: &mut Vec<Command>,
) {
    lib.button_rects.clear();
    select::sync(snapshot, lib);
    nav::show(root, snapshot, lib, commands);
    info::show(root, snapshot, thumbs, histogram, lib, commands);
    toolbar::show(root, snapshot, lib, commands);
    grid::show(
        root,
        snapshot,
        thumbs,
        lib,
        photo,
        import_requests,
        commands,
    );
    selection_bar::show(root.ctx(), snapshot, lib, commands);
    reject_dialog::show(root.ctx(), snapshot, lib, commands);
}
