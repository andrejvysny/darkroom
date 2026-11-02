//! Import-dialog source previews: decoded off-thread, never touching the catalog or the cache.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use super::{decode, Decoded, PREVIEW_CAP, PREVIEW_EDGE, PREVIEW_INFLIGHT};
use ui_egui::ThumbState;

enum Preview {
    Ready {
        handle: egui::TextureHandle,
        size: [u32; 2],
    },
    Failed,
}

type PreviewResult = (String, Result<Decoded, String>);

struct PreviewWorker {
    jobs: Option<Sender<String>>,
    results: Receiver<PreviewResult>,
    thread: Option<JoinHandle<()>>,
}

/// Embedded-preview decodes for not-yet-imported files: one lazily started thread, a small
/// recency-bounded texture cache keyed by path, no catalog or thumbnail-cache access.
pub(super) struct Previews {
    entries: HashMap<String, (Preview, u64)>,
    pending: HashSet<String>,
    worker: Option<PreviewWorker>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Previews {
    pub(super) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            entries: HashMap::new(),
            pending: HashSet::new(),
            worker: None,
            wake,
        }
    }

    pub(super) fn ask(&mut self, path: &str, frame: u64) -> ThumbState {
        if let Some((preview, last_used)) = self.entries.get_mut(path) {
            *last_used = frame;
            return match preview {
                Preview::Ready { handle, size } => ThumbState::Ready {
                    texture: handle.id(),
                    size: *size,
                },
                Preview::Failed => ThumbState::Failed,
            };
        }
        if self.pending.contains(path) {
            return ThumbState::Loading;
        }
        if self.pending.len() >= PREVIEW_INFLIGHT {
            return ThumbState::Loading;
        }
        match self.send(path) {
            Ok(()) => {
                self.pending.insert(path.to_string());
                ThumbState::Loading
            }
            Err(error) => {
                eprintln!("import preview unavailable: {error}");
                self.entries
                    .insert(path.to_string(), (Preview::Failed, frame));
                self.evict(frame);
                ThumbState::Failed
            }
        }
    }

    fn send(&mut self, path: &str) -> Result<(), String> {
        if self.worker.is_none() {
            let (jobs_tx, jobs_rx) = mpsc::channel::<String>();
            let (results_tx, results) = mpsc::channel::<PreviewResult>();
            let wake = Arc::clone(&self.wake);
            let thread = std::thread::Builder::new()
                .name("import-preview".into())
                .spawn(move || {
                    while let Ok(path) = jobs_rx.recv() {
                        let outcome = std::panic::catch_unwind(|| decode_file_preview(&path))
                            .unwrap_or_else(|payload| {
                                Err(format!(
                                    "preview worker panicked: {}",
                                    crate::photo_worker::panic_text(&*payload)
                                ))
                            });
                        if results_tx.send((path, outcome)).is_err() {
                            return;
                        }
                        wake();
                    }
                })
                .map_err(|error| format!("could not start the preview worker: {error}"))?;
            self.worker = Some(PreviewWorker {
                jobs: Some(jobs_tx),
                results,
                thread: Some(thread),
            });
        }
        self.worker
            .as_ref()
            .and_then(|worker| worker.jobs.as_ref())
            .ok_or_else(|| "preview worker stopped".to_string())?
            .send(path.to_string())
            .map_err(|_| "preview worker stopped".to_string())
    }

    pub(super) fn drain(&mut self, ctx: &egui::Context, frame: u64) {
        let Some(worker) = &self.worker else { return };
        let done: Vec<PreviewResult> = worker.results.try_iter().collect();
        for (path, outcome) in done {
            self.pending.remove(&path);
            let preview = match outcome {
                Ok(decoded) => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [decoded.size[0] as usize, decoded.size[1] as usize],
                        &decoded.rgba,
                    );
                    let handle =
                        ctx.load_texture("import-preview", image, egui::TextureOptions::LINEAR);
                    Preview::Ready {
                        handle,
                        size: decoded.size,
                    }
                }
                Err(error) => {
                    eprintln!("import preview {path}: {error}");
                    Preview::Failed
                }
            };
            self.entries.insert(path, (preview, frame));
        }
        self.evict(frame);
    }

    /// Drop the least recently asked-for entries beyond the cap (never the current frame's).
    fn evict(&mut self, frame: u64) {
        while self.entries.len() > PREVIEW_CAP {
            let victim = self
                .entries
                .iter()
                .filter(|(_, (_, last_used))| *last_used < frame)
                .min_by_key(|(_, (_, last_used))| *last_used)
                .map(|(path, _)| path.clone());
            match victim {
                Some(path) => self.entries.remove(&path),
                None => break,
            };
        }
    }

    pub(super) fn shutdown(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            worker.jobs = None; // closes the channel; the thread exits after its current decode
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
        self.pending.clear();
    }
}

impl Drop for Previews {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Decode only the embedded preview of one source file; never writes to disk or the catalog.
fn decode_file_preview(path: &str) -> Result<Decoded, String> {
    let source = core_raw::source_from_path(std::path::Path::new(path))
        .map_err(|error| format!("could not open {path}: {error}"))?;
    let thumb = core_raw::thumbnail_jpeg(&source, PREVIEW_EDGE, 82)
        .map_err(|error| format!("could not decode {path}: {error}"))?;
    decode(&thumb.jpeg)
}
