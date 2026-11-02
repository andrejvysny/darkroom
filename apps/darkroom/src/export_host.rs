//! Host side of the Export dialog: owns the [`ExportView`], executes the UI's [`ExportRequest`]s
//! (folder picker, preference persistence, starting/cancelling the worker) and folds worker events
//! back into the view. Runs between frames on the main thread, like the import requests.

use crate::export_worker::{ExportEvent, ExportItem, ExportJob, ExportWorker};
use crate::settings::SettingsState;
use core_app::export::{
    ExportPhase, ExportPreset, ExportRequest, ExportTarget, ExportView, BUILTIN_PRESETS,
};
use core_app::state::ApplicationSnapshot;
use core_model::identity::ImageId;
use core_model::settings::{set_native, ExportLast};
use core_runtime::export_io::local_ymd;
use core_runtime::import::ImportPlatform;
use std::path::PathBuf;

pub struct ExportHost {
    view: ExportView,
    worker: Option<ExportWorker>,
}

impl ExportHost {
    pub fn new(worker: Option<ExportWorker>) -> Self {
        Self {
            view: ExportView::default(),
            worker,
        }
    }

    pub fn view(&self) -> &ExportView {
        &self.view
    }

    /// Execute one request. `Err` is a message for the command-error strip (the dialog itself
    /// reports Start/preset refusals through [`ExportView::error`]).
    pub fn handle(
        &mut self,
        request: ExportRequest,
        snapshot: &ApplicationSnapshot,
        settings: &mut SettingsState,
        platform: &dyn ImportPlatform,
    ) -> Result<(), String> {
        match request {
            ExportRequest::Open(ids) => self.open(&ids, snapshot, settings)?,
            ExportRequest::Close => {
                if !self.view.busy() {
                    self.view.open = false;
                    self.view.phase = ExportPhase::Idle;
                }
            }
            ExportRequest::PickDir => self.pick_dir(settings, platform),
            ExportRequest::SetTemplate(template) if !self.view.busy() => {
                self.view.template = template;
            }
            ExportRequest::SetFormat(format) if !self.view.busy() => self.view.format = format,
            ExportRequest::SetQuality(quality) if !self.view.busy() => {
                self.view.quality = quality.clamp(1, 100);
            }
            ExportRequest::SavePreset(name) => self.save_preset(&name, settings),
            ExportRequest::DeletePreset(name) => self.delete_preset(&name, settings),
            ExportRequest::Start => self.start(snapshot, settings),
            ExportRequest::Cancel => self.cancel(),
            ExportRequest::SetTemplate(_)
            | ExportRequest::SetFormat(_)
            | ExportRequest::SetQuality(_) => {}
        }
        Ok(())
    }

    /// Fold worker progress into the view; `true` when anything changed (repaint).
    pub fn pump(&mut self) -> bool {
        let Some(worker) = self.worker.as_ref() else {
            return false;
        };
        let events = worker.drain();
        let changed = !events.is_empty();
        for event in events {
            match event {
                ExportEvent::Progress {
                    done,
                    failed,
                    total,
                } => {
                    self.view.phase = if matches!(self.view.phase, ExportPhase::Cancelling { .. }) {
                        ExportPhase::Cancelling {
                            done,
                            failed,
                            total,
                        }
                    } else {
                        ExportPhase::Running {
                            done,
                            failed,
                            total,
                        }
                    };
                }
                ExportEvent::Finished {
                    done,
                    failed,
                    first_error,
                    dir,
                } => {
                    self.view.phase = ExportPhase::Finished {
                        done,
                        failed,
                        first_error,
                        dir,
                    };
                }
            }
        }
        changed
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        self.worker.as_mut().map_or(Ok(()), ExportWorker::shutdown)
    }

    fn open(
        &mut self,
        ids: &[ImageId],
        snapshot: &ApplicationSnapshot,
        settings: &SettingsState,
    ) -> Result<(), String> {
        if self.view.busy() {
            self.view.open = true;
            return Ok(());
        }
        let targets: Vec<ExportTarget> = ids
            .iter()
            .filter_map(|id| {
                let row = snapshot
                    .library
                    .rows
                    .iter()
                    .find(|row| row.id == id.0)
                    .or_else(|| snapshot.editor.image.as_ref().filter(|row| row.id == id.0))?;
                Some(ExportTarget {
                    image: *id,
                    filename: row.filename.clone(),
                    date: local_ymd(row.capture_date.unwrap_or(row.imported_at)),
                })
            })
            .collect();
        if targets.is_empty() {
            return Err("Select a photo to export".into());
        }
        let mut view = ExportView {
            open: true,
            targets,
            ..ExportView::default()
        };
        if let Some(prefs) = settings.current() {
            view.dir = prefs.export_dir.value.as_ref().map(PathBuf::from);
            view.presets = prefs.export_presets.value.clone();
            if let Some(last) = &prefs.export_last.value {
                view.template = last.template.clone();
                view.format = last.format;
                view.quality = last.quality.clamp(1, 100);
            }
        }
        self.view = view;
        Ok(())
    }

    fn pick_dir(&mut self, settings: &mut SettingsState, platform: &dyn ImportPlatform) {
        if self.view.busy() {
            return;
        }
        let Some(dir) = platform.pick_folder("Export to…", self.view.dir.as_deref()) else {
            return;
        };
        let saved = dir.to_string_lossy().into_owned();
        self.view.dir = Some(dir);
        self.view.error = settings
            .update(|prefs, now| set_native(&mut prefs.export_dir, Some(saved), now))
            .err();
    }

    fn save_preset(&mut self, name: &str, settings: &mut SettingsState) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if BUILTIN_PRESETS.iter().any(|(builtin, _)| *builtin == name) {
            self.view.error = Some(format!("\"{name}\" is a built-in preset name"));
            return;
        }
        let mut presets = self.view.presets.clone();
        presets.retain(|preset| preset.name != name);
        presets.push(ExportPreset {
            name: name.to_owned(),
            template: self.view.template.clone(),
        });
        self.store_presets(presets, settings);
    }

    fn delete_preset(&mut self, name: &str, settings: &mut SettingsState) {
        let mut presets = self.view.presets.clone();
        presets.retain(|preset| preset.name != name);
        self.store_presets(presets, settings);
    }

    fn store_presets(&mut self, presets: Vec<ExportPreset>, settings: &mut SettingsState) {
        let saved = presets.clone();
        match settings.update(|prefs, now| set_native(&mut prefs.export_presets, saved, now)) {
            Ok(()) => {
                self.view.presets = presets;
                self.view.error = None;
            }
            Err(error) => self.view.error = Some(error),
        }
    }

    fn start(&mut self, snapshot: &ApplicationSnapshot, settings: &mut SettingsState) {
        if self.view.busy() {
            return;
        }
        let refusal = self.refusal();
        if let Some(reason) = refusal {
            self.view.error = Some(reason);
            return;
        }
        let (Some(worker), Some(dir)) = (self.worker.as_ref(), self.view.dir.clone()) else {
            return;
        };
        let items = self
            .view
            .targets
            .iter()
            .zip(self.view.planned_names())
            .map(|(target, file_name)| ExportItem {
                image: target.image,
                file_name,
            })
            .collect();
        let open_doc = snapshot
            .editor
            .selected
            .zip(snapshot.editor.document.params.clone());
        let total = self.view.targets.len();
        let job = ExportJob {
            items,
            dir,
            format: self.view.format,
            quality: self.view.quality,
            open_doc,
        };
        if let Err(error) = worker.start_job(job) {
            self.view.error = Some(error);
            return;
        }
        let last = ExportLast {
            template: self.view.template.clone(),
            format: self.view.format,
            quality: self.view.quality,
        };
        // Remembering the choice is a convenience: a failed save must not stop the export.
        if let Err(error) =
            settings.update(|prefs, now| set_native(&mut prefs.export_last, Some(last), now))
        {
            eprintln!("{error}");
        }
        self.view.error = None;
        self.view.phase = ExportPhase::Running {
            done: 0,
            failed: 0,
            total,
        };
    }

    /// Why Start must not run now.
    fn refusal(&self) -> Option<String> {
        if self.worker.is_none() {
            return Some("Export is unavailable: the GPU renderer did not start".into());
        }
        if self.view.targets.is_empty() {
            return Some("Select a photo to export".into());
        }
        if self.view.template.trim().is_empty() {
            return Some("Enter a filename template".into());
        }
        match &self.view.dir {
            Some(dir) if dir.is_dir() => None,
            Some(dir) => Some(format!("{} is not a folder", dir.display())),
            None => Some("Choose a destination folder".into()),
        }
    }

    fn cancel(&mut self) {
        if let ExportPhase::Running {
            done,
            failed,
            total,
        } = self.view.phase
        {
            if let Some(worker) = &self.worker {
                worker.cancel();
            }
            self.view.phase = ExportPhase::Cancelling {
                done,
                failed,
                total,
            };
        }
    }
}
