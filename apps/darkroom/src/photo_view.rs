//! Photo presentation bookkeeping for the native window.
//!
//! The editor snapshot is the single source of truth for *which* photo is shown. Every frame
//! [`PhotoView::reconcile`] compares the presented/wanted frame against the snapshot's subject
//! (catalog epoch, image, editor session, selection generation) and retires anything that no
//! longer matches, so an old photo can never stay visible under a new selection, a loading
//! document or a closed catalog.
//!
//! Frame order in the host: `reconcile` → `accept_results` → `ui_state` → egui `run_ui` →
//! `submit` → paint + queue submit → `after_submit`.

use crate::open_journey::{Journeys, Milestone};
use crate::photo_worker::{self, PhotoRequest, PhotoResult, WarmRequest};
use core_app::state::{ApplicationSnapshot, DocumentStatus};
use core_model::identity::QualityTier;
use core_model::identity::{
    CatalogEpoch, DeviceEpoch, EditorSession, FrameHandle, FrameKey, ImageId, SelectionGeneration,
};
use core_pipeline::ViewParams;
use render_wgpu::{FrameLease, Offer, Presenter};
use std::collections::VecDeque;
use std::time::Instant;
use ui_egui::{HistogramData, PhotoRequestPlan, PhotoUi, Picture, PresentId};

/// A crop-local UV window `(origin, size)`, as the plan carries it.
type Window = ([f32; 2], [f32; 2]);
/// What a submitted request will be, remembered until its frame is presented: key, identity,
/// window, and whether the texture is sampled nearest-neighbour.
type Submission = (FrameKey, PresentId, Option<Window>, bool);
/// Requests whose window/identity are remembered until their frame is presented.
const SUBMITTED_CAP: usize = 16;

mod neighbours;
mod placeholder;
#[cfg(test)]
mod tests;

/// egui registration of leased textures, abstracted so tests need no `egui_wgpu::Renderer`.
pub trait TextureRegistry {
    /// `nearest`: sample with nearest-neighbour (a native-resolution window magnified past 100 %).
    fn register(&mut self, lease: &FrameLease, nearest: bool) -> (egui::TextureId, [u32; 2]);
    fn free(&mut self, id: egui::TextureId);
}

/// The real registry: the host's egui renderer on the shared device.
pub struct EguiRegistry<'a> {
    pub device: &'a wgpu::Device,
    pub renderer: &'a mut egui_wgpu::Renderer,
}

impl TextureRegistry for EguiRegistry<'_> {
    fn register(&mut self, lease: &FrameLease, nearest: bool) -> (egui::TextureId, [u32; 2]) {
        crate::present::register_presented_frame(self.device, self.renderer, lease, nearest)
    }

    fn free(&mut self, id: egui::TextureId) {
        self.renderer.free_texture(&id);
    }
}

/// What a frame is *of*: every identity field of a [`FrameKey`] except the edit revision and
/// the viewport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhotoSubject {
    pub catalog_epoch: CatalogEpoch,
    pub image_id: ImageId,
    pub editor_session: EditorSession,
    pub selection_generation: SelectionGeneration,
}

impl PhotoSubject {
    pub fn of_key(key: &FrameKey) -> Self {
        let identity = &key.identity;
        Self {
            catalog_epoch: identity.source.catalog_epoch,
            image_id: identity.source.image_id,
            editor_session: identity.editor_session,
            selection_generation: identity.selection_generation,
        }
    }

    /// `None` with the catalog closed or nothing selected.
    pub fn of_snapshot(snapshot: &ApplicationSnapshot) -> Option<Self> {
        if !snapshot.catalog_ready {
            return None;
        }
        Some(Self {
            catalog_epoch: snapshot.catalog_epoch,
            image_id: snapshot.editor.selected?,
            editor_session: snapshot.editor.editor_session,
            selection_generation: snapshot.editor.selection_generation,
        })
    }
}

/// True when the snapshot holds a document this host can develop right now.
pub fn renderable(snapshot: &ApplicationSnapshot) -> bool {
    let editor = &snapshot.editor;
    let Some(image) = editor.image.as_ref() else {
        return false;
    };
    let document = &editor.document;
    Some(ImageId(image.id)) == editor.selected
        && document.status == DocumentStatus::Ready
        && document.revision.is_some()
        && document.process_version.is_some()
        && document
            .params
            .as_deref()
            .is_some_and(|params| photo_worker::unsupported_reason(params).is_none())
}

struct Registration {
    texture: egui::TextureId,
    handle: FrameHandle,
    size: [u32; 2],
    subject: PhotoSubject,
    /// What the frame is of and the window it shows, from the request that produced it.
    present: Option<PresentId>,
    window: Option<Window>,
}

#[derive(Default)]
pub struct PhotoView {
    presenter: Presenter,
    registration: Option<Registration>,
    wanted: Option<FrameKey>,
    failure: Option<(FrameKey, String)>,
    /// Persistent host-level reason photos cannot be rendered at all (worker missing or dead).
    unavailable: Option<String>,
    pending_free: Vec<egui::TextureId>,
    pending_leases: Vec<FrameLease>,
    device_epoch: DeviceEpoch,
    /// Newest histogram of the wanted subject; dropped whenever the subject is retired.
    histogram: Option<HistogramData>,
    /// Open-latency milestones of the photo being opened (diagnostics only).
    journeys: Journeys,
    /// The source last handed to the worker as a warm-up, so each selection warms once.
    warmed: Option<(i64, String)>,
    /// The selected source whose neighbours were last handed to the prefetch.
    pub(super) prefetched_for: Option<(i64, String)>,
    /// Identity + window of recent submissions, matched to their frame when it is presented.
    submitted: VecDeque<Submission>,
    /// The cached picture under the live frame.
    placeholders: placeholder::Placeholders,
}

impl PhotoView {
    pub fn new(device_epoch: DeviceEpoch) -> Self {
        Self {
            device_epoch,
            ..Self::default()
        }
    }

    /// A new photo was selected: start timing how long it takes to appear.
    pub fn begin_journey(&mut self, image: ImageId) {
        self.journeys.begin(image, Instant::now());
    }

    /// A warm-up for the selected photo as soon as its catalog row is known — before its edit
    /// document is ready — so the decode overlaps the document load. At most once per source.
    pub fn warm_request(&mut self, snapshot: &ApplicationSnapshot) -> Option<WarmRequest> {
        if !snapshot.catalog_ready || self.unavailable.is_some() {
            return None;
        }
        let editor = &snapshot.editor;
        let image = editor.image.as_ref()?;
        if editor.selected != Some(ImageId(image.id)) {
            return None;
        }
        // A document already known to be unrenderable never needs its pixels.
        if !matches!(
            editor.document.status,
            DocumentStatus::Loading | DocumentStatus::Ready
        ) {
            return None;
        }
        let source = (image.id, image.content_hash.clone());
        if self.warmed.as_ref() == Some(&source) {
            return None;
        }
        self.warmed = Some(source);
        Some(WarmRequest {
            image_id: image.id,
            path: std::path::PathBuf::from(&image.path),
            content_hash: image.content_hash.clone(),
        })
    }

    /// Enable disk previews as placeholders (without it only resident thumbnails are used).
    pub fn attach_stage_preview(&mut self, loader: crate::stage_preview::StagePreview) {
        self.placeholders.attach(loader);
    }

    /// Refresh the placeholder for the selection (before `ui_state`). `edge` is the configured
    /// preview long edge, read lazily: only when a disk preview is actually requested.
    pub fn update_placeholder(
        &mut self,
        snapshot: &ApplicationSnapshot,
        ctx: &egui::Context,
        thumbs: Option<&mut crate::thumbs::ThumbStore>,
        edge: &dyn Fn() -> u32,
    ) {
        if let Some(image) = self.placeholders.update(snapshot, ctx, thumbs, edge) {
            self.journeys
                .mark(image, Milestone::Placeholder, Instant::now());
        }
    }

    /// The photo worker is missing or dead: show `message` instead of a spinner, forever.
    pub fn set_unavailable(&mut self, message: String) {
        self.unavailable.get_or_insert(message);
    }

    /// Retire everything that no longer belongs to the snapshot's subject. Nothing is freed
    /// here: retirements wait for [`Self::after_submit`] so one rule covers every path.
    pub fn reconcile(&mut self, snapshot: &ApplicationSnapshot) {
        let subject = PhotoSubject::of_snapshot(snapshot).filter(|_| renderable(snapshot));
        let keep = match (subject, &self.wanted) {
            (Some(subject), Some(wanted)) => PhotoSubject::of_key(wanted) == subject,
            (Some(_), None) => true,
            (None, _) => false,
        };
        if !keep {
            self.retire_all();
        }
    }

    /// Publish finished worker results through the presenter.
    pub fn accept_results(&mut self, results: Vec<PhotoResult>, reg: &mut dyn TextureRegistry) {
        for result in results {
            match result {
                PhotoResult::Frame(lease) => self.accept_frame(lease, reg),
                PhotoResult::Histogram { key, r, g, b } => {
                    // Only a histogram of the subject still wanted is kept (the newest wins).
                    if self.wanted.as_ref().map(PhotoSubject::of_key)
                        == Some(PhotoSubject::of_key(&key))
                    {
                        self.histogram = Some(HistogramData {
                            image: key.identity.source.image_id,
                            r,
                            g,
                            b,
                        });
                    }
                }
                // Worker death is handled by the host (`WorkerStatus`).
                PhotoResult::WorkerDied(_) => {}
                // Normally superseded (wanted already moved on). If it is still wanted, forget
                // it so the next frame resubmits instead of waiting on dropped work.
                PhotoResult::Abandoned(key) => {
                    if self.wanted == Some(key) {
                        self.wanted = None;
                    }
                }
                PhotoResult::Failed { key, message } => {
                    // A failure of a request nobody wants any more is dropped, never shown.
                    if self.wanted == Some(key) {
                        self.failure = Some((key, message));
                    }
                }
            }
        }
    }

    fn accept_frame(&mut self, lease: FrameLease, reg: &mut dyn TextureRegistry) {
        let handle = lease.handle();
        let subject = PhotoSubject::of_key(lease.key());
        let quality = lease.key().quality;
        let key = *lease.key();
        // Newest submission of this key wins (a key re-submitted later carries the same
        // window, but the newest is the one still recorded after eviction).
        let sent = self.submitted.iter().rev().find(|(k, ..)| *k == key);
        let (present, window) = (sent.map(|s| s.1), sent.and_then(|s| s.2));
        let nearest = sent.is_some_and(|s| s.3);
        let (texture, size) = reg.register(&lease, nearest);
        if self.presenter.offer(lease, self.device_epoch) == Offer::Presented {
            if let Some(previous) = self.registration.replace(Registration {
                texture,
                handle,
                size,
                subject,
                present,
                window,
            }) {
                self.pending_free.push(previous.texture);
            }
            let milestone = match quality {
                QualityTier::Settled => Milestone::Settled,
                QualityTier::Interactive => Milestone::Live,
            };
            self.journeys
                .mark(subject.image_id, milestone, Instant::now());
            self.collect();
        } else {
            // A rejected lease was never in a draw list.
            reg.free(texture);
        }
    }

    /// Central-panel state for this frame, derived from the snapshot.
    pub fn ui_state(&self, snapshot: &ApplicationSnapshot) -> PhotoUi {
        let subject = PhotoSubject::of_snapshot(snapshot);
        let message = self
            .unavailable
            .clone()
            .or_else(|| document_message(snapshot));
        let failure = self
            .failure
            .as_ref()
            .filter(|(key, _)| {
                self.wanted == Some(*key) && Some(PhotoSubject::of_key(key)) == subject
            })
            .map(|(_, error)| match snapshot.editor.image.as_ref() {
                // The missing-file message already names the path.
                Some(image) if !error.starts_with(crate::presence::MISSING_SOURCE) => {
                    format!("{error} — {}", image.filename)
                }
                _ => error.clone(),
            });
        let texture = self
            .registration
            .as_ref()
            .filter(|reg| Some(reg.subject) == subject)
            .map(|reg| match reg.present {
                Some(id) => Picture {
                    texture: reg.texture,
                    px: reg.size,
                    id,
                    window: reg.window,
                },
                None => Picture::bare(reg.texture, reg.size),
            });
        let presented = self.presenter.current().map(|lease| lease.key());
        let satisfied = self.wanted.as_ref().is_some_and(|w| Some(w) == presented);
        // A selection whose catalog row is still loading is loading too (never "Select a photo").
        let row_pending = snapshot.editor.selected.is_some() && snapshot.editor.image.is_none();
        let loading = message.is_none()
            && failure.is_none()
            && (row_pending
                || snapshot.editor.image.is_some()
                    && match snapshot.editor.document.status {
                        DocumentStatus::Loading => true,
                        DocumentStatus::Ready => !satisfied,
                        _ => false,
                    });
        let placeholder = self
            .placeholders
            .picture(subject.map(|subject| subject.image_id));
        PhotoUi {
            texture,
            placeholder,
            message,
            failure,
            retry: false,
            loading,
            device_epoch: self.device_epoch,
            plan: None,
        }
    }

    /// Histogram of the presented subject, or `None` (never another photo's distribution).
    pub fn histogram(&self, snapshot: &ApplicationSnapshot) -> Option<&HistogramData> {
        let subject = PhotoSubject::of_snapshot(snapshot)?;
        let histogram = self.histogram.as_ref()?;
        let wanted = self.wanted.as_ref().map(PhotoSubject::of_key)?;
        (wanted == subject && histogram.image == subject.image_id).then_some(histogram)
    }

    /// After `run_ui`: turn the UI's plan/retry into a worker request (or nothing). A failed key
    /// is never resubmitted automatically; only an explicit retry does.
    pub fn submit(&mut self, plan: Option<PhotoRequestPlan>, retry: bool) -> Option<PhotoRequest> {
        if retry {
            self.failure = None;
            self.wanted = None;
        }
        let plan = plan?;
        if self.wanted == Some(plan.key) {
            return None;
        }
        self.wanted = Some(plan.key);
        self.failure = None;
        self.placeholders.variant = plan.present.variant;
        if self.submitted.len() == SUBMITTED_CAP {
            self.submitted.pop_front();
        }
        self.submitted
            .push_back((plan.key, plan.present, plan.window, plan.nearest));
        self.presenter.want(plan.key);
        self.sync_registration();
        self.collect();
        Some(PhotoRequest {
            key: plan.key,
            path: plan.path,
            content_hash: plan.content_hash,
            params: plan.params,
            view: ViewParams {
                crop_preview: plan.crop_preview,
                ..match plan.window {
                    Some((origin, size)) => photo_worker::window_view(
                        origin,
                        size,
                        plan.viewport_px.0,
                        plan.viewport_px.1,
                    ),
                    None => photo_worker::fit_view(plan.viewport_px.0, plan.viewport_px.1),
                }
            },
        })
    }

    /// Call right after the egui draw list was submitted to the queue. The draw list built by
    /// `run_ui` may reference a texture whose lease was retired this frame; freeing the egui
    /// registration or returning the lease to the pool earlier would let a draw sample a freed
    /// texture or a pool reuse overwrite it. Pool reuse after this point is ordered after the
    /// sampling draw by the same queue.
    pub fn after_submit(&mut self, reg: &mut dyn TextureRegistry) {
        for id in self.pending_free.drain(..) {
            reg.free(id);
        }
        self.pending_leases.clear();
    }

    /// Device/surface death: nothing is painted afterwards, so release immediately.
    pub fn retire_for_device_loss(&mut self, reg: &mut dyn TextureRegistry) {
        self.retire_all();
        self.placeholders.retire();
        self.after_submit(reg);
    }

    fn retire_all(&mut self) {
        self.presenter.retire_all();
        self.wanted = None;
        self.failure = None;
        self.histogram = None;
        self.sync_registration();
        self.collect();
    }

    fn collect(&mut self) {
        self.pending_leases.extend(self.presenter.take_retired());
    }

    /// The registration follows the presenter's current lease; anything else is queued to free.
    fn sync_registration(&mut self) {
        let presented = self.presenter.current().map(|lease| lease.handle());
        if self
            .registration
            .as_ref()
            .is_some_and(|reg| Some(reg.handle) != presented)
        {
            if let Some(reg) = self.registration.take() {
                self.pending_free.push(reg.texture);
            }
        }
    }
}

/// Document-level state that replaces the image (and suppresses the plan): unsupported or
/// unreadable edits must never be silently shown as a render.
fn document_message(snapshot: &ApplicationSnapshot) -> Option<String> {
    snapshot.editor.image.as_ref()?;
    let document = &snapshot.editor.document;
    match &document.status {
        DocumentStatus::Ready => {
            let feature = photo_worker::unsupported_reason(document.params.as_deref()?)?;
            Some(format!(
                "Darkroom can't render this photo yet: {feature} is not supported by this \
                 build — nothing was changed."
            ))
        }
        DocumentStatus::Unsupported(reason) => Some(format!(
            "Darkroom can't read this photo's saved adjustments ({reason}) — nothing was changed."
        )),
        DocumentStatus::Corrupt => Some(
            "Darkroom can't read this photo's saved adjustments — the saved edit is not \
             readable. Nothing was changed."
                .to_string(),
        ),
        DocumentStatus::Error(error) => Some(format!(
            "Darkroom could not load this photo's adjustments: {error}. Nothing was changed."
        )),
        DocumentStatus::Loading | DocumentStatus::Empty => None,
    }
}
