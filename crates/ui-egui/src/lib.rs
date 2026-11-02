//! Native egui panels. Pure presentation: reads an immutable [`ApplicationSnapshot`], holds
//! transient UI input state ([`UiState`]) and emits typed [`Command`]s. No GPU, no I/O, no
//! runtime — the host owns window, device/queue, thumbnail decoding and platform integration and
//! reaches the UI only through [`Frame`] (snapshot, presented photo, [`ThumbSource`], histogram).

mod develop;
mod histogram;
pub mod icons;
mod import_dialog;
mod library;
mod panels;
mod photo;
mod shell;
pub mod theme;
pub mod widgets;

pub use develop::{DevelopUi, DEVELOP_PANEL_ID, EXPOSURE_SLIDER_ID, SAVE_ERROR_ID};
pub use import_dialog::{ImportUi, IMPORT_DIALOG_ID};
pub use library::{LibraryUi, GRID_SCROLL_ID, INFO_ID, NAV_ID, NOT_YET, TOOLBAR_ID};
pub use panels::{settings_notice_strip, SETTINGS_NOTICES_ID};
pub use photo::{cropped_frame_aspect, fit_output, viewport_revision, PhotoRequestPlan, PhotoUi};
pub use shell::{COMMAND_ERROR_ID, SEARCH_ID, TOP_BAR_ID};

use core_app::import::{ImportRequest, ImportView};
use core_app::state::ApplicationSnapshot;
use core_app::workflow::Command;
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use std::time::Instant;

/// Top-level workspace mode (the segmented control in the top bar).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Mode {
    #[default]
    Library,
    Develop,
    /// Not migrated yet: shown as an explicit placeholder, never a fake screen.
    Duplicates,
}

/// All transient UI state the host keeps between frames. Never authoritative document, catalog
/// or settings state — that lives in the snapshot.
#[derive(Default)]
pub struct UiState {
    pub mode: Mode,
    pub develop: DevelopUi,
    /// Horizontal space (points) the top bar leaves free on the left for platform window
    /// controls drawn over the content (macOS traffic lights with a transparent titlebar).
    pub chrome_inset_left: f32,
    pub library: LibraryUi,
    pub import: ImportUi,
    /// Import requests emitted this frame; the host drains and executes them.
    pub import_requests: Vec<ImportRequest>,
}

/// Per-channel 256-bin histogram of the developed frame currently on screen. The host passes it
/// only when it belongs to the presented subject (same image/editor session), so the panel never
/// shows another photo's distribution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistogramData {
    pub image: ImageId,
    pub r: Vec<u32>,
    pub g: Vec<u32>,
    pub b: Vec<u32>,
}

/// What the host can show for one grid/filmstrip cell right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbState {
    /// Uploaded egui texture with its pixel size (already orientation-corrected).
    Ready {
        texture: egui::TextureId,
        size: [u32; 2],
    },
    /// Requested; draw a neutral placeholder.
    Loading,
    /// The thumbnail could not be produced (unsupported/missing/corrupt source).
    Failed,
}

/// Host-implemented thumbnail lookup. The UI calls [`ThumbSource::thumb`] for every cell it lays
/// out this frame, in visual priority order (the first call is the most important); the host
/// treats the set of rows asked for in one frame as the visible set, queues missing ones and may
/// evict textures that were not asked for. Calls must be cheap (no I/O on the UI thread).
pub trait ThumbSource {
    fn thumb(&mut self, image: &ImageRow) -> ThumbState;
    /// Preview of a not-yet-imported source file (`path` comes from [`ImportView::files`]).
    /// Same contract as [`ThumbSource::thumb`]: cheap, queued, never blocking.
    fn file_preview(&mut self, _path: &str) -> ThumbState {
        ThumbState::Failed
    }
}

/// A [`ThumbSource`] with nothing to show (headless tests, no-catalog window).
pub struct NoThumbs;
impl ThumbSource for NoThumbs {
    fn thumb(&mut self, _image: &ImageRow) -> ThumbState {
        ThumbState::Loading
    }
}

/// Everything the host lends the UI for one frame.
pub struct Frame<'a> {
    pub snapshot: &'a ApplicationSnapshot,
    /// The command the Open Catalog action dispatches (host knows the path).
    pub reopen: &'a Command,
    /// Last UI-command dispatch error (see `State::last_command_error` in the host's
    /// `window.rs`); shown dismissibly, set to `None` when dismissed.
    pub command_error: &'a mut Option<(String, Instant)>,
    pub photo: &'a mut PhotoUi,
    /// Startup settings diagnostics (see [`settings_notice_strip`]).
    pub settings_notices: &'a mut Vec<String>,
    pub thumbs: &'a mut dyn ThumbSource,
    pub histogram: Option<&'a HistogramData>,
    /// Import service state (dialog, progress, last report).
    pub import: &'a ImportView,
}

/// The root UI.
pub fn root(root: &mut egui::Ui, frame: Frame<'_>, state: &mut UiState) -> Vec<Command> {
    let Frame {
        snapshot,
        reopen,
        command_error,
        photo,
        settings_notices,
        thumbs,
        histogram,
        import,
    } = frame;
    theme::ensure(root.ctx());
    // Captured before any widget runs: a single-line TextEdit surrenders focus while handling
    // Enter/Esc, so by the time the global key handlers run the key that ended the edit would
    // otherwise be read as a shortcut (Enter → Develop, Cmd+Z → photo undo).
    let typing = root.ctx().text_edit_focused();
    // Modals block pointer input only; their keys must not reach the grid behind them (a culling
    // key under the delete-rejected confirmation would change what the confirm trashes).
    let modal_open = import.open || state.library.modal_open();
    let mut commands = Vec::new();
    if !typing && !modal_open {
        develop::shortcuts(
            root.ctx(),
            develop::enabled_context(snapshot),
            &mut commands,
        );
    }
    shell::top_bar(root, snapshot, reopen, state, &mut commands);
    shell::command_error_strip(root, command_error);
    settings_notice_strip(root, settings_notices);
    develop::error_banner(root, snapshot, &mut state.develop, &mut commands);
    // The photo plan is only meaningful while the Develop canvas is on screen.
    photo.plan = None;
    match state.mode {
        Mode::Library => library::show(
            root,
            snapshot,
            thumbs,
            histogram,
            &mut state.library,
            photo,
            &mut state.import_requests,
            &mut commands,
        ),
        Mode::Develop => {
            develop::show(
                root,
                snapshot,
                &mut state.develop,
                thumbs,
                histogram,
                photo,
                &mut commands,
            );
        }
        Mode::Duplicates => shell::duplicates_placeholder(root),
    }
    import_dialog::show(
        root.ctx(),
        snapshot,
        import,
        thumbs,
        &mut state.import,
        &mut state.import_requests,
        &mut commands,
    );
    library::handle_keys(
        root.ctx(),
        typing || modal_open,
        snapshot,
        &mut state.library,
        &mut state.mode,
        &mut commands,
    );
    commands
}
