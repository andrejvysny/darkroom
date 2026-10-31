use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::registry::{action, ActionId, EnabledContext};
use core_app::state::ApplicationSnapshot;
use core_app::subscription::{Notice, SnapshotUpdate};
use core_app::workflow::Command;
use core_app::AppError;
use core_model::identity::ImageId;
use core_runtime::async_workflow::AsyncCatalogWorkflow;
use core_runtime::workflow::{InlineExecutor, RuntimePaths};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

struct SystemClock;
impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
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

pub fn ui(root: &mut egui::Ui, snapshot: &ApplicationSnapshot, reopen: &Command) -> Vec<Command> {
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
        });
    });
    egui::Panel::right("catalog_metadata").show(root, |ui| metadata(ui, snapshot));
    egui::CentralPanel::default().show(root, |ui| {
        if let Some(error) = &snapshot.error {
            ui.label(error.to_string());
        }
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

pub fn probe(path: PathBuf) -> Result<serde_json::Value, AppError> {
    let mut app = NativeApplication::new(path, false, Arc::new(|| {}))?;
    app.wait_idle()?;
    let initial = app.snapshot();
    if let Some(first) = initial.library.rows.first() {
        app.dispatch(Command::SelectImage(Some(ImageId(first.id))))?;
        app.wait_idle()?;
    }
    let snapshot = app.snapshot();
    let result = serde_json::json!({"status":"ok","catalog_epoch":snapshot.catalog_epoch.0,"count":snapshot.library.count,"rows":snapshot.library.rows.as_ref(),"selected":snapshot.editor.image});
    app.shutdown()?;
    app.wait_idle()?;
    app.pump()?;
    Ok(result)
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
