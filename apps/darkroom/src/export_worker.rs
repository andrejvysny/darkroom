//! Sequential full-resolution export worker.
//!
//! One background thread runs one [`ExportJob`] at a time, photo by photo: decode -> GPU render ->
//! border composite -> encode -> never-overwrite write. Cancel is checked between photos and
//! between the stages of a photo; a failing photo is counted and the batch continues. The
//! production body (`export_one`) is injected into the loop so tests drive ordering, cancel and
//! failure handling without a GPU.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use core_app::export::ExportFormat;
use core_model::identity::ImageId;
use core_model::DevelopParams;

#[path = "export_worker/render.rs"]
mod render;

pub use render::ExportDeps;

/// One photo of a job with its already de-duplicated output file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportItem {
    pub image: ImageId,
    pub file_name: String,
}

#[derive(Clone, Debug)]
pub struct ExportJob {
    pub items: Vec<ExportItem>,
    pub dir: PathBuf,
    pub format: ExportFormat,
    pub quality: u8,
    /// The photo open in Develop with its in-memory (possibly unsaved) edit: exported as shown.
    pub open_doc: Option<(ImageId, Arc<DevelopParams>)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Written,
    /// Cancel was seen at a stage boundary; nothing was written.
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportEvent {
    /// One photo finished (written or failed).
    Progress {
        done: usize,
        failed: usize,
        total: usize,
    },
    Finished {
        done: usize,
        failed: usize,
        first_error: Option<String>,
        dir: PathBuf,
    },
}

/// One photo's work; the production body renders on the GPU, tests inject their own.
pub type ExportBody =
    dyn Fn(&ExportJob, &ExportItem, &AtomicBool) -> Result<Outcome, String> + Send + Sync + 'static;

pub struct ExportWorker {
    jobs: Option<Sender<ExportJob>>,
    events: Receiver<ExportEvent>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ExportWorker {
    pub fn production(deps: ExportDeps, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let body: Arc<ExportBody> =
            Arc::new(move |job, item, cancel| render::export_one(&deps, job, item, cancel));
        Self::start(body, wake)
    }

    pub fn start(body: Arc<ExportBody>, wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let (jobs, job_rx) = mpsc::channel::<ExportJob>();
        let (tx, events) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let thread = std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                for job in job_rx {
                    worker_cancel.store(false, Ordering::SeqCst);
                    run_job(&job, &*body, &worker_cancel, &tx, &*wake);
                }
            })
            .map_err(|error| format!("could not start the export worker: {error}"))?;
        Ok(Self {
            jobs: Some(jobs),
            events,
            cancel,
            thread: Some(thread),
        })
    }

    /// Queue a job. The host starts one at a time (its view is `Running` until `Finished`).
    pub fn start_job(&self, job: ExportJob) -> Result<(), String> {
        self.jobs
            .as_ref()
            .ok_or_else(|| "the export worker has stopped".to_owned())?
            .send(job)
            .map_err(|_| "the export worker has stopped".to_owned())
    }

    /// Skip every photo not yet started and stop the running one at its next stage boundary.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn drain(&self) -> Vec<ExportEvent> {
        self.events.try_iter().collect()
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::SeqCst);
        self.jobs = None;
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err("the export worker thread panicked".into()),
            _ => Ok(()),
        }
    }
}

impl Drop for ExportWorker {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("{error}");
        }
    }
}

fn run_job(
    job: &ExportJob,
    body: &ExportBody,
    cancel: &AtomicBool,
    tx: &Sender<ExportEvent>,
    wake: &(dyn Fn() + Send + Sync),
) {
    let total = job.items.len();
    let (mut done, mut failed) = (0, 0);
    let mut first_error = None;
    for item in &job.items {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(job, item, cancel)))
                .unwrap_or_else(|payload| {
                    Err(format!(
                        "export panicked: {}",
                        crate::photo_worker::panic_text(&*payload)
                    ))
                });
        match outcome {
            Ok(Outcome::Written) => done += 1,
            Ok(Outcome::Cancelled) => break,
            Err(error) => {
                failed += 1;
                first_error.get_or_insert(error);
            }
        }
        if tx
            .send(ExportEvent::Progress {
                done,
                failed,
                total,
            })
            .is_err()
        {
            return; // host gone
        }
        wake();
    }
    let _ = tx.send(ExportEvent::Finished {
        done,
        failed,
        first_error,
        dir: job.dir.clone(),
    });
    wake();
}

#[cfg(test)]
#[path = "export_worker/tests.rs"]
mod tests;
