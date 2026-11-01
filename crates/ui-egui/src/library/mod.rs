//! Library mode: catalog nav (left), virtualised thumbnail grid (centre), info panel (right),
//! toolbar (bottom) and the culling keyboard. Everything derives from the snapshot; every change
//! is a [`Command`].

mod grid;
mod info;
mod keys;
mod nav;
mod toolbar;

use crate::{HistogramData, Mode, ThumbSource};
use core_app::registry::{action, ActionId};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use core_model::catalog::{ImageRow, QueryParams};
use core_model::identity::{CatalogEpoch, ImageId};
use std::collections::HashSet;

pub(crate) use keys::handle as handle_keys;

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

pub(crate) fn show(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    thumbs: &mut dyn ThumbSource,
    histogram: Option<&HistogramData>,
    lib: &mut LibraryUi,
    mode: &mut Mode,
    commands: &mut Vec<Command>,
) {
    nav::show(root, snapshot, lib, commands);
    info::show(root, snapshot, thumbs, histogram, commands);
    toolbar::show(root, snapshot, lib, commands);
    grid::show(root, snapshot, thumbs, lib, mode, commands);
}
