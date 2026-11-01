use crate::photo_worker;
use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::registry::{action, ActionId, EnabledContext};
use core_app::state::{ApplicationSnapshot, DocumentStatus};
use core_app::subscription::{Notice, SnapshotUpdate};
use core_app::workflow::Command;
use core_app::AppError;
use core_model::identity::{
    CacheVersion, DeviceEpoch, DisplayRevision, FrameKey, ImageId, RenderIdentity, RendererVersion,
    SourceIdentity, SourceRevision, ViewportRevision,
};
use core_model::{Crop, DevelopParams};
use core_pipeline::ViewParams;
use core_runtime::async_workflow::AsyncCatalogWorkflow;
use core_runtime::workflow::{InlineExecutor, RuntimePaths};
use render_wgpu::QualityTier;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::Instant,
};

struct SystemClock;
impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        crate::settings::now_ms()
    }
}
struct NativeEvents;
impl EventSink for NativeEvents {
    fn published(&self, update: &SnapshotUpdate) {
        if let Notice::EffectCompleted {
            result: Err(error), ..
        } = &update.notice
        {
            eprintln!("catalog operation failed: {error}");
        }
    }
}
struct RenderCapability(bool);
impl RenderBackend for RenderCapability {
    fn available(&self) -> bool {
        self.0
    }
}

pub struct NativeApplication {
    workflow: AsyncCatalogWorkflow,
    catalog_path: PathBuf,
}
impl NativeApplication {
    pub fn new(
        path: PathBuf,
        renderer_available: bool,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, AppError> {
        let mut workflow = AsyncCatalogWorkflow::new(
            RuntimePaths {
                catalog: path.clone(),
            },
            Arc::new(SystemClock),
            Arc::new(NativeEvents),
            Arc::new(RenderCapability(renderer_available)),
            Arc::new(InlineExecutor),
            wake,
            core_runtime::jobs::Config {
                workers: 2,
                max_jobs: 16,
                max_bytes: 128 * 1024 * 1024,
                max_terminal_records: 128,
            },
        )?;
        workflow.open_initial()?;
        Ok(Self {
            workflow,
            catalog_path: path,
        })
    }
    pub fn reopen_command(&self) -> Command {
        Command::OpenCatalog(self.catalog_path.clone())
    }
    pub fn snapshot(&self) -> Arc<ApplicationSnapshot> {
        self.workflow.snapshot()
    }
    pub fn dispatch(&mut self, command: Command) -> Result<(), AppError> {
        self.workflow.dispatch(command)
    }
    pub fn pump(&mut self) -> Result<(), AppError> {
        while self.workflow.pump()? {}
        if self.workflow.needs_join() {
            self.workflow.join_shutdown();
        }
        Ok(())
    }
    pub fn is_shutdown_complete(&self) -> bool {
        self.workflow.is_shutdown_complete()
    }
    pub fn wait_idle(&mut self) -> Result<(), AppError> {
        self.workflow.wait_idle()
    }
    pub fn shutdown(&mut self) -> Result<(), AppError> {
        self.dispatch(Command::RequestShutdown)
    }
}

/// A render plan the host must submit: `key` identifies the frame, the rest is the
/// `PhotoRequest` payload for the photo worker. Computed by the central-panel draw (the only
/// place the panel's rect is known) and consumed by the host after `run_ui`.
pub struct PhotoPlan {
    pub key: FrameKey,
    pub path: PathBuf,
    pub content_hash: String,
    pub params: Arc<DevelopParams>,
    pub view: ViewParams,
}

/// Photo presentation state for the central panel, owned by the host (`window.rs`): the egui
/// texture of the presented frame, the status label, and the out-param for this frame's plan.
pub struct PhotoUi {
    /// Registered egui texture of the frame the presenter currently holds, with its pixel size.
    pub texture: Option<(egui::TextureId, [u32; 2])>,
    /// Explicit state label (protected/unsupported/failed). When set it replaces the image —
    /// an unreadable or unsupported document must never be silently shown as a render.
    pub message: Option<String>,
    /// A frame is wanted but not yet presented (or absent at all).
    pub loading: bool,
    /// Device epoch stamped into every wanted key (the host's renderer epoch).
    pub device_epoch: DeviceEpoch,
    /// Filled by the central-panel draw when a frame should be rendered at the panel size.
    pub plan: Option<PhotoPlan>,
}

/// The largest size with the frame's `frame_aspect` (W/H) that fits inside `panel_px`, each
/// side >= 1, rounded. The fit view: the panel is the bounding box, the frame keeps its aspect,
/// and the leftover panel area is background (the image is drawn centered, never stretched).
///
/// Degenerate input is clamped, never panicked: panel sides < 1 become 1; a non-finite or
/// non-positive aspect falls back to the clamped panel size (nothing to fit against).
pub fn fit_output(panel_px: (u32, u32), frame_aspect: f64) -> (u32, u32) {
    let panel_w = panel_px.0.max(1);
    let panel_h = panel_px.1.max(1);
    if !frame_aspect.is_finite() || frame_aspect <= 0.0 {
        return (panel_w, panel_h);
    }
    if f64::from(panel_w) >= f64::from(panel_h) * frame_aspect {
        // Height-bound: full panel height, width from the aspect (<= panel_w by the guard).
        let width = (f64::from(panel_h) * frame_aspect)
            .round()
            .clamp(1.0, f64::from(panel_w)) as u32;
        (width, panel_h)
    } else {
        // Width-bound: full panel width, height from the aspect.
        let height = (f64::from(panel_w) / frame_aspect)
            .round()
            .clamp(1.0, f64::from(panel_h)) as u32;
        (panel_w, height)
    }
}

/// Aspect (W/H) of what the pipeline outputs for this edit: the CROPPED frame's aspect, from
/// the same crop-aware output dims the legacy develop viewport uses —
/// [`Crop::output_size`] over the image's display dims (incl. rot90; the catalog stores the
/// oriented display dims, which is exactly what `develop_linear` uprights to). Missing/zero
/// catalog dims (legacy rows) fall back to `fallback` (no correction), never a bogus aspect.
fn cropped_frame_aspect(
    crop: &Crop,
    width: Option<i64>,
    height: Option<i64>,
    fallback: f64,
) -> f64 {
    let (width, height) = match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w as u32, h as u32),
        _ => return fallback,
    };
    let (frame_w, frame_h) = crop.output_size(width, height);
    f64::from(frame_w) / f64::from(frame_h)
}

/// Stable per-process hash of the viewport pixel size (fit view: no zoom yet, so the geometry
/// revision depends on nothing else).
fn viewport_revision(width: u32, height: u32) -> ViewportRevision {
    let mut hasher = DefaultHasher::new();
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    ViewportRevision(hasher.finish())
}

/// The wanted frame for a `Ready` document at this panel rect, or `None` when nothing is
/// renderable (no selection, document not `Ready`). Protection (denoise/AI masks, corrupt,
/// unreadable) is enforced by the host through `PhotoUi::message` + the worker's own check; a
/// plan is only ever produced for params the worker accepts.
fn photo_plan(
    snapshot: &ApplicationSnapshot,
    rect: egui::Rect,
    ppp: f32,
    device_epoch: DeviceEpoch,
) -> Option<PhotoPlan> {
    let image = snapshot.editor.image.as_ref()?;
    if snapshot.editor.document.status != DocumentStatus::Ready {
        return None;
    }
    let params = snapshot.editor.document.params.clone()?;
    let revision = snapshot.editor.document.revision?;
    let process_version = snapshot.editor.document.process_version?;
    let panel_w = (rect.width() * ppp).round().max(1.0) as u32;
    let panel_h = (rect.height() * ppp).round().max(1.0) as u32;
    // Aspect-correct fit view: the render target is the largest size with the CROPPED frame's
    // aspect that fits the panel — drawing it 1:1 centered never stretches it to the panel.
    let aspect = cropped_frame_aspect(
        &params.crop,
        image.width,
        image.height,
        f64::from(panel_w) / f64::from(panel_h),
    );
    let (width, height) = fit_output((panel_w, panel_h), aspect);
    // The content hash rides in the request (the prepared-cache key); the identity pins the
    // source revision, which a same-id re-index reflects there, not here.
    let key = FrameKey {
        identity: RenderIdentity {
            source: SourceIdentity {
                catalog_epoch: snapshot.catalog_epoch,
                image_id: ImageId(image.id),
                source_revision: SourceRevision(0),
            },
            editor_session: snapshot.editor.editor_session,
            selection_generation: snapshot.editor.selection_generation,
            edit_revision: revision,
            process_version,
            renderer_version: RendererVersion(1),
            cache_version: CacheVersion(1),
            viewport_revision: viewport_revision(width, height),
            display_revision: DisplayRevision(0),
            device_epoch,
        },
        viewport_px: (width, height),
        scale_milli: (ppp * 1000.0).round() as u32,
        quality: QualityTier::Settled,
    };
    Some(PhotoPlan {
        key,
        path: PathBuf::from(&image.path),
        content_hash: image.content_hash.clone(),
        params,
        view: photo_worker::fit_view(width, height),
    })
}

/// Status-area strip for the settings diagnostics collected at startup
/// (`crate::settings::SettingsState::load`): a corrupt file repaired, a settings file written by
/// a newer build, legacy keys the bridge refused. Same red-label style as the command error, in
/// its own row under the top bar; the Dismiss button clears the whole strip. Renders nothing when
/// there is nothing to report, so the ordinary layout is unchanged.
pub fn settings_notice_strip(root: &mut egui::Ui, notices: &mut Vec<String>) {
    if notices.is_empty() {
        return;
    }
    egui::Panel::top("settings_notices").show(root, |ui| {
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

/// The root UI. `command_error` is the last UI-command dispatch error (see
/// `State::last_command_error` in `window.rs`); it is shown as a dismissible
/// red label in the status area and set to `None` when the user dismisses it.
/// `settings_notices` are the startup settings diagnostics (see [`settings_notice_strip`]).
pub fn ui(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    reopen: &Command,
    command_error: &mut Option<(String, Instant)>,
    photo: &mut PhotoUi,
    settings_notices: &mut Vec<String>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    let context = EnabledContext {
        running: snapshot.running,
        catalog_ready: snapshot.catalog_ready,
        has_selection: snapshot.editor.selected.is_some(),
    };
    egui::Panel::top("catalog_actions").show(root, |ui| {
        ui.horizontal(|ui| {
            ui.heading("Darkroom");
            if snapshot.jobs.metrics.queued + snapshot.jobs.metrics.running > 0 {
                ui.spinner();
            }
            for (id, command) in [
                (ActionId::OpenCatalog, reopen.clone()),
                (ActionId::RefreshLibrary, Command::RefreshLibrary),
                (ActionId::CloseCatalog, Command::CloseCatalog),
            ] {
                let descriptor = action(id);
                if ui
                    .add_enabled(
                        descriptor.enabled.is_enabled(context),
                        egui::Button::new(descriptor.label),
                    )
                    .clicked()
                {
                    commands.push(command);
                }
            }
            pagination(ui, snapshot, &mut commands);
            if let Some(message) = command_error.as_ref().map(|(message, _)| message.clone()) {
                ui.colored_label(egui::Color32::RED, &message);
                if ui.small_button("Dismiss").clicked() {
                    *command_error = None;
                }
            }
        });
    });
    settings_notice_strip(root, settings_notices);
    egui::Panel::right("catalog_metadata").show(root, |ui| metadata(ui, snapshot));
    egui::Panel::left("library_list").show(root, |ui| {
        egui::ScrollArea::vertical().show_rows(
            ui,
            22.0,
            snapshot.library.rows.len(),
            |ui, range| {
                for index in range {
                    let row = &snapshot.library.rows[index];
                    if ui
                        .selectable_label(
                            snapshot.editor.selected == Some(ImageId(row.id)),
                            &row.filename,
                        )
                        .clicked()
                    {
                        commands.push(Command::SelectImage(Some(ImageId(row.id))));
                    }
                }
            },
        );
    });
    egui::CentralPanel::default().show(root, |ui| {
        if let Some(error) = &snapshot.error {
            ui.label(error.to_string());
        }
        // The panel rect in points is only known here; the plan carries it as physical pixels.
        let rect = ui.available_rect_before_wrap();
        photo.plan = if photo.message.is_some() {
            None
        } else {
            photo_plan(
                snapshot,
                rect,
                ui.ctx().pixels_per_point(),
                photo.device_epoch,
            )
        };
        if let Some(message) = &photo.message {
            // A protected/unsupported/failed state names itself; never show a stale render
            // of another state under it.
            ui.label(message);
        } else if let Some((texture, size)) = photo.texture {
            // The frame was developed at the aspect-correct fitted size: draw it at exactly
            // that size (points = px / pixels_per_point) centered in the panel; the rest of the
            // panel stays the background color. The rect is never resized here — `ui.put`
            // stretches the texture to the given rect — so a stale frame from a larger previous
            // panel merely overflows, hidden by the panel's clip until the refit lands.
            let ppp = ui.ctx().pixels_per_point().max(f32::EPSILON);
            let size_points = egui::vec2(size[0].max(1) as f32 / ppp, size[1].max(1) as f32 / ppp);
            let draw_rect = egui::Rect::from_center_size(rect.center(), size_points);
            let image =
                egui::Image::from_texture(egui::load::SizedTexture::new(texture, size_points))
                    .fit_to_exact_size(size_points);
            ui.put(draw_rect, image);
            if photo.loading {
                ui.spinner();
            }
        } else if photo.loading {
            ui.spinner();
            ui.label("Loading photo…");
        } else if snapshot.editor.image.is_none() {
            ui.label("Select a photo");
        }
        // Claim the panel area for this frame's layout regardless of what was drawn in it.
        ui.allocate_rect(rect, egui::Sense::hover());
    });
    commands
}

fn metadata(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot) {
    ui.heading("Photo information");
    if let Some(image) = &snapshot.editor.image {
        ui.label(&image.filename);
        ui.label(&image.path);
        for value in [
            &image.camera_make,
            &image.camera_model,
            &image.lens,
            &image.format,
        ]
        .into_iter()
        .flatten()
        {
            ui.label(value);
        }
        if let (Some(w), Some(h)) = (image.width, image.height) {
            ui.label(format!("{w} × {h}"));
        }
        if let Some(iso) = image.iso {
            ui.label(format!("ISO {iso}"));
        }
        if let Some(aperture) = image.aperture {
            ui.label(format!("f/{aperture}"));
        }
        if let Some(shutter) = &image.shutter {
            ui.label(shutter);
        }
        ui.label(format!("{} stars", image.stars));
    } else {
        ui.label("Select a photo");
    }
}

/// Why a `--catalog-probe` run failed. Each variant maps to a distinct process exit code
/// (see the table in `main.rs`); `kind` is the machine-readable failure category printed
/// in the error JSON on stdout.
#[derive(Debug)]
pub enum ProbeError {
    /// The catalog never became ready: open failed (corrupt file, schema written by a newer
    /// build, unwritable path) or readiness was never reported.
    Open { kind: &'static str, error: String },
    /// The selection effect failed, or the editor did not end up holding exactly the row
    /// the library query reported (id / filename / content hash must match).
    Selection { error: String },
    /// `RequestShutdown` did not run the runtime to completion (`join_shutdown` never happened).
    Shutdown { error: String },
    /// The catalog opened, but a subsequent read (library query) failed.
    Query { error: String },
    /// The photo worker could not produce a frame (decode, GPU, or timeout), or the produced
    /// frame did not match the offscreen reference render byte-for-byte.
    Render { error: String },
    /// The selected document is not renderable by this slice: not `Ready`, or an edit the SDR
    /// host refuses (AI denoise / AI masks).
    Document { error: String },
}
impl ProbeError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Open { kind, .. } => kind,
            Self::Selection { .. } => "selection",
            Self::Shutdown { .. } => "shutdown",
            Self::Query { .. } => "query",
            Self::Render { .. } => "render",
            Self::Document { .. } => "document",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::Open { error, .. }
            | Self::Selection { error, .. }
            | Self::Shutdown { error, .. }
            | Self::Query { error, .. }
            | Self::Render { error, .. }
            | Self::Document { error, .. } => error,
        }
    }
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Open { .. } => 2,
            Self::Selection { .. } => 3,
            Self::Shutdown { .. } => 4,
            Self::Query { .. } => 5,
            Self::Render { .. } => 6,
            Self::Document { .. } => 7,
        }
    }
}

/// Machine-readable category of an open-time `AppError` (subset of the `AppError` enum;
/// everything not listed is generic catalog IO).
fn app_error_kind(error: &AppError) -> &'static str {
    match error {
        AppError::SchemaTooNew { .. } => "schema_too_new",
        AppError::Corrupt(_) => "corrupt",
        AppError::NotOpen => "not_open",
        _ => "catalog",
    }
}

fn open_error(error: AppError) -> ProbeError {
    ProbeError::Open {
        kind: app_error_kind(&error),
        error: error.to_string(),
    }
}

/// Headless catalog health check. Unlike a naive snapshot dump, every stage must positively
/// prove itself: readiness after `wait_idle`, the selected row matching the queried row, and
/// a completed runtime shutdown — any unproven stage is an `Err`, never a success JSON.
pub fn probe(path: PathBuf, select_first: bool) -> Result<serde_json::Value, ProbeError> {
    let mut app = NativeApplication::new(path, false, Arc::new(|| {})).map_err(open_error)?;
    app.wait_idle().map_err(open_error)?;
    let initial = app.snapshot();
    if !initial.catalog_ready {
        return Err(match initial.error.as_ref() {
            Some(error) => open_error(error.clone()),
            None => ProbeError::Open {
                kind: "not_open",
                error: "catalog did not become ready".into(),
            },
        });
    }
    // Open succeeded but a follow-up read (library query / folders) failed: distinct from
    // an open failure so callers can tell "damaged catalog" from "unreadable contents".
    if let Some(error) = initial.error.as_ref() {
        return Err(ProbeError::Query {
            error: error.to_string(),
        });
    }

    let mut selected = serde_json::Value::Null;
    if let Some(first) = initial.library.rows.first().cloned() {
        if select_first {
            let selection_error = |error: AppError| ProbeError::Selection {
                error: error.to_string(),
            };
            app.dispatch(Command::SelectImage(Some(ImageId(first.id))))
                .map_err(selection_error)?;
            app.wait_idle().map_err(selection_error)?;
            let after = app.snapshot();
            if let Some(error) = after.error.as_ref() {
                return Err(ProbeError::Selection {
                    error: error.to_string(),
                });
            }
            match after.editor.image.as_ref() {
                None => {
                    return Err(ProbeError::Selection {
                        error: format!("editor holds no image after selecting id {}", first.id),
                    })
                }
                Some(image) => {
                    if image.id != first.id
                        || image.filename != first.filename
                        || image.content_hash != first.content_hash
                    {
                        return Err(ProbeError::Selection {
                            error: format!(
                                "selected image {:?} does not match library row {} ({}, {})",
                                (image.id, &image.filename, &image.content_hash),
                                first.id,
                                first.filename,
                                first.content_hash
                            ),
                        });
                    }
                    selected = serde_json::to_value(image).map_err(|e| ProbeError::Selection {
                        error: format!("selected image is not serializable: {e}"),
                    })?;
                }
            }
        }
    }

    let snapshot = app.snapshot();
    let result = serde_json::json!({
        "status": "ok",
        "catalog_epoch": snapshot.catalog_epoch.0,
        "count": snapshot.library.count,
        "empty": snapshot.library.count == 0 && snapshot.library.rows.is_empty(),
        "rows": snapshot.library.rows.as_ref(),
        "selected": selected,
    });

    let shutdown_error = |error: AppError| ProbeError::Shutdown {
        error: error.to_string(),
    };
    app.shutdown().map_err(shutdown_error)?;
    app.wait_idle().map_err(shutdown_error)?;
    // `pump` drains completed work and calls the runtime's `join_shutdown()` on the terminal
    // state, the same way the window host's `about_to_wait` exit path does.
    app.pump().map_err(shutdown_error)?;
    if !app.is_shutdown_complete() {
        return Err(ProbeError::Shutdown {
            error: "runtime shutdown did not complete".into(),
        });
    }
    Ok(result)
}

/// Fixed panel (physical px) the probe fits the frame into — deliberately SQUARE so an aspect
/// defect cannot hide: a 3:2 frame must come back 3:2-fitted, never panel-shaped. Fixed so runs
/// are comparable; clamped by the host code paths themselves (targets far below any device
/// limit).
const PHOTO_PROBE_PANEL: (u32, u32) = (1024, 1024);
const PHOTO_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Headless proof that native real-photo presentation is byte-exact: open the catalog, select an
/// image, require the document is `Ready`, then run the SAME `photo_worker` GPU path on a
/// surfaceless device and compare the presented frame, byte-for-byte (tolerance 0), against the
/// offscreen `DevelopPipeline::render_view` for the same prepared image, params and view.
pub fn photo_probe(
    path: PathBuf,
    image_id: Option<ImageId>,
) -> Result<serde_json::Value, ProbeError> {
    let mut app = NativeApplication::new(path, true, Arc::new(|| {})).map_err(open_error)?;
    app.wait_idle().map_err(open_error)?;
    let initial = app.snapshot();
    if !initial.catalog_ready {
        return Err(match initial.error.as_ref() {
            Some(error) => open_error(error.clone()),
            None => ProbeError::Open {
                kind: "not_open",
                error: "catalog did not become ready".into(),
            },
        });
    }
    if let Some(error) = initial.error.as_ref() {
        return Err(ProbeError::Query {
            error: error.to_string(),
        });
    }

    let selection_error = |error: AppError| ProbeError::Selection {
        error: error.to_string(),
    };
    let target = match image_id {
        Some(id) => Some(id),
        None => initial.library.rows.first().map(|row| ImageId(row.id)),
    };
    let Some(id) = target else {
        return Err(ProbeError::Document {
            error: "catalog has no images to render".into(),
        });
    };
    app.dispatch(Command::SelectImage(Some(id)))
        .map_err(selection_error)?;
    app.wait_idle().map_err(selection_error)?;
    let snapshot = app.snapshot();
    if let Some(error) = snapshot.error.as_ref() {
        return Err(ProbeError::Selection {
            error: error.to_string(),
        });
    }
    let image = snapshot
        .editor
        .image
        .as_ref()
        .filter(|row| ImageId(row.id) == id)
        .ok_or_else(|| ProbeError::Selection {
            error: format!("editor does not hold image {}", id.0),
        })?;
    if snapshot.editor.document.status != DocumentStatus::Ready {
        return Err(ProbeError::Document {
            error: format!(
                "document is {:?}, not Ready",
                snapshot.editor.document.status
            ),
        });
    }
    let params = snapshot
        .editor
        .document
        .params
        .clone()
        .ok_or_else(|| ProbeError::Document {
            error: "Ready document carries no params".into(),
        })?;
    if let Some(feature) = photo_worker::unsupported_reason(&params) {
        return Err(ProbeError::Document {
            error: format!(
                "edit uses unsupported feature: {feature} — Darkroom can't render this yet"
            ),
        });
    }
    let revision = snapshot
        .editor
        .document
        .revision
        .ok_or_else(|| ProbeError::Document {
            error: "Ready document carries no revision".into(),
        })?;
    let process_version =
        snapshot
            .editor
            .document
            .process_version
            .ok_or_else(|| ProbeError::Document {
                error: "Ready document carries no process version".into(),
            })?;

    // Surfaceless GPU — same construction the `--gpu-probe` uses.
    let gpu = crate::gpu::Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle()),
        None,
    )
    .map_err(|error| ProbeError::Render {
        error: format!("GPU unavailable: {error}"),
    })?;
    let device_epoch = DeviceEpoch(1);
    let source_path = PathBuf::from(&image.path);

    // Decode the reference once up front. Its true frame dims are the aspect ground truth —
    // what the pipeline outputs for this edit is the CROPPED frame of THIS decode — while the
    // wanted size mirrors the window's planning path (catalog display dims + crop, fitted into
    // the fixed square panel). The aspect check below compares the two derivations, so the
    // reported `aspect` is never a tautology of a key the probe built itself: catalog-dims
    // drift from the real decode fails the probe.
    let mut reference_params = (*params).clone();
    reference_params.display_referred = core_raw::is_display(&source_path);
    let source = core_raw::source_from_path(&source_path).map_err(|error| ProbeError::Render {
        error: format!("reference decode failed: {error}"),
    })?;
    let linear = core_raw::develop_linear(&source).map_err(|error| ProbeError::Render {
        error: format!("reference develop failed: {error}"),
    })?;
    let expected_aspect = cropped_frame_aspect(
        &params.crop,
        Some(i64::from(linear.width)),
        Some(i64::from(linear.height)),
        1.0,
    );
    let planned_aspect = cropped_frame_aspect(&params.crop, image.width, image.height, 1.0);
    let (width, height) = fit_output(PHOTO_PROBE_PANEL, planned_aspect);
    let key = FrameKey {
        identity: RenderIdentity {
            source: SourceIdentity {
                catalog_epoch: snapshot.catalog_epoch,
                image_id: id,
                source_revision: SourceRevision(0),
            },
            editor_session: snapshot.editor.editor_session,
            selection_generation: snapshot.editor.selection_generation,
            edit_revision: revision,
            process_version,
            renderer_version: RendererVersion(1),
            cache_version: CacheVersion(1),
            viewport_revision: viewport_revision(width, height),
            display_revision: DisplayRevision(0),
            device_epoch,
        },
        viewport_px: (width, height),
        scale_milli: 1000,
        quality: QualityTier::Settled,
    };
    let view = photo_worker::fit_view(width, height);

    let mut worker = photo_worker::PhotoWorker::start(
        &gpu.context,
        Arc::clone(&gpu.pipeline),
        device_epoch,
        Arc::new(|| {}),
    );
    worker.request(photo_worker::PhotoRequest {
        key,
        path: source_path.clone(),
        content_hash: image.content_hash.clone(),
        params: Arc::clone(&params),
        view,
    });

    // A diagnostic renderer over the same device: `read_back` is explicit and never used by the
    // presentation path itself.
    let reader = render_wgpu::Renderer::new(
        core_pipeline::GpuContext {
            device: gpu.context.device.clone(),
            queue: gpu.context.queue.clone(),
            info: gpu.context.info.clone(),
            max_texture_dim: gpu.context.max_texture_dim,
        },
        Arc::clone(&gpu.pipeline),
        device_epoch,
        render_wgpu::PoolLimits::default(),
    );

    let deadline = Instant::now() + PHOTO_PROBE_TIMEOUT;
    let mut outcome: Option<Result<render_wgpu::FrameLease, ProbeError>> = None;
    while outcome.is_none() {
        for result in worker.drain_results() {
            outcome = Some(match result {
                photo_worker::PhotoResult::Frame(lease) => Ok(lease),
                photo_worker::PhotoResult::Failed { message, .. } => {
                    Err(ProbeError::Render { error: message })
                }
            });
            if outcome.is_some() {
                break;
            }
        }
        if outcome.is_none() {
            if Instant::now() >= deadline {
                outcome = Some(Err(ProbeError::Render {
                    error: format!(
                        "photo worker produced no frame within {}s",
                        PHOTO_PROBE_TIMEOUT.as_secs()
                    ),
                }));
            } else {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
    worker.shutdown();
    let lease = outcome.expect("loop ends with an outcome")?;

    // Aspect proof: the presented frame must carry the cropped frame's aspect, not the panel's
    // (the squashed-presentation regression this probe exists to catch). Tolerance is the
    // rounding of the short side to integer pixels: |aspect - expected| <= 1/min(w, h).
    let (frame_w, frame_h) = lease.key().viewport_px;
    let aspect = f64::from(frame_w) / f64::from(frame_h);
    let tolerance = 1.0 / f64::from(frame_w.min(frame_h).max(1));
    if (aspect - expected_aspect).abs() > tolerance {
        return Err(ProbeError::Render {
            error: format!(
                "presented frame is not aspect-correct: {frame_w}×{frame_h} (aspect \
                 {aspect:.6}) does not match the cropped frame aspect {expected_aspect:.6} \
                 within {tolerance:.6}"
            ),
        });
    }

    let native = reader
        .read_back(&lease)
        .map_err(|error| ProbeError::Render {
            error: format!("readback failed: {error}"),
        })?;

    // Offscreen reference: prepare + `render_view` on the same decode with identical inputs.
    let reference = {
        let prepared = gpu
            .pipeline
            .prepare(&gpu.context, &linear)
            .map_err(|error| ProbeError::Render {
                error: format!("reference prepare failed: {error}"),
            })?;
        gpu.pipeline
            .render_view(&gpu.context, &prepared, &reference_params, &view)
            .map_err(|error| ProbeError::Render {
                error: format!("reference render failed: {error}"),
            })?
    };

    let identical = native == reference;
    let mean_rgb = if native.is_empty() {
        [0.0; 3]
    } else {
        let mut sums = [0u64; 3];
        for pixel in native.chunks_exact(4) {
            for channel in 0..3 {
                sums[channel] += u64::from(pixel[channel]);
            }
        }
        let count = u64::try_from(native.len() / 4).unwrap_or(1).max(1);
        std::array::from_fn(|channel| sums[channel] as f64 / count as f64)
    };

    let shutdown_error = |error: AppError| ProbeError::Shutdown {
        error: error.to_string(),
    };
    // Present the produced frame through the window's exact egui path — same registration
    // helper, one egui frame painted into an offscreen target inside a Validation error scope
    // (see `present.rs`). This is what the read-back-only probe could not see: the real-window
    // crash where egui's `egui_user_image_0` bind group was rejected because the pooled target
    // lacked TEXTURE_BINDING. Any validation error is a render failure (exit 6).
    crate::present::paint_probe_frame(&gpu, &lease)
        .map_err(|error| ProbeError::Render { error })?;
    app.shutdown().map_err(shutdown_error)?;
    app.wait_idle().map_err(shutdown_error)?;
    app.pump().map_err(shutdown_error)?;
    if !app.is_shutdown_complete() {
        return Err(ProbeError::Shutdown {
            error: "runtime shutdown did not complete".into(),
        });
    }

    Ok(serde_json::json!({
        "status": if identical { "ok" } else { "mismatch" },
        "image_id": image.id,
        // Read from the delivered lease, not the requested key: the aspect fields describe
        // the frame that actually arrived.
        "width": frame_w,
        "height": frame_h,
        "aspect": aspect,
        "expected_aspect": expected_aspect,
        "mean_rgb": mean_rgb,
        "identical": identical,
        "egui_presented": true,
    }))
}

fn pagination(ui: &mut egui::Ui, snapshot: &ApplicationSnapshot, commands: &mut Vec<Command>) {
    let query = &snapshot.library.query;
    let offset = query.offset.unwrap_or(0).max(0);
    let limit = query.limit.unwrap_or(5000).max(1);
    let loaded = i64::try_from(snapshot.library.rows.len()).unwrap_or(i64::MAX);
    ui.label(format!(
        "{}–{} of {} photos",
        if loaded == 0 { 0 } else { offset + 1 },
        offset.saturating_add(loaded),
        snapshot.library.count
    ));
    for (label, next, enabled) in [
        ("Previous", offset.saturating_sub(limit).max(0), offset > 0),
        (
            "Next",
            offset.saturating_add(limit),
            offset.saturating_add(loaded) < snapshot.library.count,
        ),
    ] {
        if ui
            .add_enabled(snapshot.catalog_ready && enabled, egui::Button::new(label))
            .clicked()
        {
            let mut query = query.clone();
            query.offset = Some(next);
            query.limit = Some(limit);
            commands.push(Command::QueryLibrary(Box::new(query)));
        }
    }
}

#[cfg(test)]
mod fit_tests {
    use super::{cropped_frame_aspect, fit_output};
    use core_model::Crop;

    const FIXTURE_ASPECT: f64 = 6960.0 / 4640.0;

    #[test]
    fn landscape_frame_into_tall_panel_is_width_bound() {
        // A 3:2 photo in the 820×625-panel's-tall analogue (625 wide, 820 tall): the width
        // caps, height comes from the aspect: round(625 / 1.5) = 417.
        assert_eq!(fit_output((625, 820), FIXTURE_ASPECT), (625, 417));
    }

    #[test]
    fn portrait_frame_into_wide_panel_is_height_bound() {
        // The same photo rotated (2:3) in a wide panel: the height caps, width = round(625·⅔).
        assert_eq!(fit_output((820, 625), 1.0 / FIXTURE_ASPECT), (417, 625));
    }

    #[test]
    fn landscape_frame_into_wide_panel_is_height_bound() {
        // The reported defect: 3:2 into 1024×1024 must be width-bound at full width, NOT the
        // panel itself (that is the squashed-presentation regression).
        assert_eq!(fit_output((1024, 1024), FIXTURE_ASPECT), (1024, 683));
        // 820×625 panel, 3:2 photo: height-bound (820 >= 625·1.5 = 937.5 is false →
        // width-bound actually): 820 < 937.5, so full width, height = round(820/1.5) = 547.
        assert_eq!(fit_output((820, 625), FIXTURE_ASPECT), (820, 547));
    }

    #[test]
    fn exact_fit_is_the_panel_itself() {
        assert_eq!(fit_output((1024, 683), 1024.0 / 683.0), (1024, 683));
        assert_eq!(fit_output((640, 480), 4.0 / 3.0), (640, 480));
        assert_eq!(fit_output((1024, 1024), 1.0), (1024, 1024));
    }

    #[test]
    fn one_pixel_panels_never_yield_a_zero_side() {
        assert_eq!(fit_output((1, 1), FIXTURE_ASPECT), (1, 1));
        assert_eq!(fit_output((1, 1), 1.0 / FIXTURE_ASPECT), (1, 1));
        // Rounded short side would be 0 → clamped to 1.
        assert_eq!(fit_output((820, 1), FIXTURE_ASPECT), (2, 1));
        assert_eq!(fit_output((1, 820), FIXTURE_ASPECT), (1, 1));
    }

    #[test]
    fn extreme_aspects_clamp_instead_of_collapsing_or_overflowing() {
        assert_eq!(fit_output((100, 100), 1.0e9), (100, 1));
        assert_eq!(fit_output((100, 100), 1.0e-9), (1, 100));
        // Degenerate aspects fall back to the clamped panel (nothing to fit against).
        assert_eq!(fit_output((10, 5), 0.0), (10, 5));
        assert_eq!(fit_output((10, 5), -1.5), (10, 5));
        assert_eq!(fit_output((10, 5), f64::NAN), (10, 5));
        assert_eq!(fit_output((0, 0), FIXTURE_ASPECT), (1, 1));
    }

    #[test]
    fn cropped_aspect_uses_crop_dims_including_rot90() {
        // Full frame: the plain display aspect.
        let full = Crop::default();
        assert!((cropped_frame_aspect(&full, Some(6960), Some(4640), -1.0) - 1.5).abs() < 1e-12);
        // Square-crop of a 3:2 frame: half-width, full height → W == H.
        let square = Crop {
            hw: 1.0 / 3.0,
            hh: 0.5,
            ..Default::default()
        };
        assert!(
            (cropped_frame_aspect(&square, Some(6960), Some(4640), -1.0) - 1.0).abs() < 1e-6,
            "hw·W must equal hh·H for a square crop"
        );
        // rot90 swaps the frame's axes.
        let rot = Crop {
            rot90: 1,
            ..Default::default()
        };
        assert!(
            (cropped_frame_aspect(&rot, Some(6960), Some(4640), -1.0) - 2.0 / 3.0).abs() < 1e-12
        );
        // Missing/zero catalog dims fall back to the caller's aspect, never 0/NaN.
        assert_eq!(cropped_frame_aspect(&full, None, Some(4640), 1.25), 1.25);
        assert_eq!(cropped_frame_aspect(&full, Some(0), Some(4640), 1.25), 1.25);
    }
}
