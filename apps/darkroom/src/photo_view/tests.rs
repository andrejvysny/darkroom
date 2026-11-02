//! `PhotoView` against real leases on a real (tiny) GPU render; egui is replaced by a registry
//! that records every register/free so lifetime ordering is observable.
//!
//! GPU convention (same as render-wgpu's tests): no adapter skips locally, but
//! `DARKROOM_REQUIRE_GPU=1` / `DARKROOM_REQUIRE_FIXTURES=1` make it a hard failure.

use super::*;
use core_app::state::{DocumentView, EditorSnapshot};
use core_model::catalog::ImageRow;
use core_model::identity::{
    CacheVersion, DisplayRevision, EditRevision, ProcessVersion, QualityTier, RenderIdentity,
    RendererVersion, SourceIdentity, SourceRevision, ViewportRevision,
};
use core_pipeline::{DevelopParams, DevelopPipeline, GpuContext, PipelineError};
use render_wgpu::{PoolLimits, Renderer};
use std::sync::Arc;

const VIEWPORT: (u32, u32) = (32, 24);

fn gpu() -> Option<GpuContext> {
    match GpuContext::new() {
        Ok(ctx) => Some(ctx),
        Err(PipelineError::NoAdapter)
            if std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_none()
                && std::env::var_os("DARKROOM_REQUIRE_GPU").is_none() =>
        {
            eprintln!("no GPU adapter — skipping (set DARKROOM_REQUIRE_GPU=1 to require one)");
            None
        }
        Err(error) => panic!("GPU initialization failed: {error}"),
    }
}

struct Harness {
    renderer: Renderer,
    prepared: core_pipeline::backend::PreparedImage,
}

fn harness() -> Option<Harness> {
    let ctx = gpu()?;
    let pipeline = Arc::new(DevelopPipeline::new(&ctx));
    let image = core_raw::LinearImage {
        width: 8,
        height: 6,
        data: (0..8 * 6 * 3).map(|i| i as f32 / 144.0).collect(),
    };
    let prepared = pipeline.prepare(&ctx, &image).unwrap();
    let renderer = Renderer::new(ctx, pipeline, DeviceEpoch(1), PoolLimits::default());
    Some(Harness { renderer, prepared })
}

impl Harness {
    fn lease(&self, key: FrameKey) -> FrameLease {
        self.renderer
            .render(
                key,
                &self.prepared,
                &DevelopParams::default(),
                &photo_worker::fit_view(VIEWPORT.0, VIEWPORT.1),
            )
            .unwrap()
    }

    fn frame(&self, key: FrameKey) -> PhotoResult {
        PhotoResult::Frame(self.lease(key))
    }

    fn live(&self) -> usize {
        self.renderer
            .pool()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .live_count(VIEWPORT)
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Ev {
    Register(egui::TextureId),
    Free(egui::TextureId),
}

#[derive(Default)]
struct FakeRegistry {
    next: u64,
    events: Vec<Ev>,
    /// Texture referenced by the draw list currently being built; freeing it is a bug.
    in_draw_list: Option<egui::TextureId>,
}

impl TextureRegistry for FakeRegistry {
    fn register(&mut self, lease: &FrameLease) -> (egui::TextureId, [u32; 2]) {
        self.next += 1;
        let id = egui::TextureId::User(self.next);
        self.events.push(Ev::Register(id));
        let (w, h) = lease.key().viewport_px;
        (id, [w, h])
    }

    fn free(&mut self, id: egui::TextureId) {
        assert_ne!(
            self.in_draw_list,
            Some(id),
            "texture freed while the pending draw list still references it"
        );
        self.events.push(Ev::Free(id));
    }
}

impl FakeRegistry {
    fn freed(&self, id: egui::TextureId) -> bool {
        self.events.contains(&Ev::Free(id))
    }
}

#[derive(Clone, Copy)]
struct Sel {
    epoch: u64,
    image: i64,
    session: u64,
    generation: u64,
}

fn sel(image: i64, generation: u64) -> Sel {
    Sel {
        epoch: 1,
        image,
        session: generation,
        generation,
    }
}

fn key(sel: Sel, revision: u64) -> FrameKey {
    FrameKey {
        identity: RenderIdentity {
            source: SourceIdentity {
                catalog_epoch: CatalogEpoch(sel.epoch),
                image_id: ImageId(sel.image),
                source_revision: SourceRevision(0),
            },
            editor_session: EditorSession(sel.session),
            selection_generation: SelectionGeneration(sel.generation),
            edit_revision: EditRevision(revision),
            process_version: ProcessVersion(6),
            renderer_version: RendererVersion(1),
            cache_version: CacheVersion(1),
            viewport_revision: ViewportRevision(0),
            display_revision: DisplayRevision(0),
            device_epoch: DeviceEpoch(1),
        },
        viewport_px: VIEWPORT,
        scale_milli: 1000,
        quality: QualityTier::Settled,
    }
}

fn snapshot(sel: Sel, status: DocumentStatus) -> ApplicationSnapshot {
    let ready = status == DocumentStatus::Ready;
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        catalog_epoch: CatalogEpoch(sel.epoch),
        library: Default::default(),
        editor: EditorSnapshot {
            selected: Some(ImageId(sel.image)),
            image: Some(ImageRow {
                id: sel.image,
                filename: format!("img{}.cr3", sel.image),
                ..Default::default()
            }),
            editor_session: EditorSession(sel.session),
            selection_generation: SelectionGeneration(sel.generation),
            document: DocumentView {
                status,
                params: ready.then(|| Arc::new(DevelopParams::default())),
                revision: ready.then_some(EditRevision(1)),
                process_version: ready.then_some(ProcessVersion(6)),
                dirty: false,
                ..Default::default()
            },
            ..Default::default()
        },
        jobs: Default::default(),
        settings: Default::default(),
        error: None,
    }
}

fn ready(sel: Sel) -> ApplicationSnapshot {
    snapshot(sel, DocumentStatus::Ready)
}

fn closed() -> ApplicationSnapshot {
    let mut snap = ready(sel(1, 1));
    snap.catalog_ready = false;
    snap.editor = EditorSnapshot::default();
    snap
}

/// What the central panel would plan for this snapshot.
fn plan(sel: Sel, snap: &ApplicationSnapshot) -> Option<PhotoRequestPlan> {
    if document_message(snap).is_some() || !renderable(snap) {
        return None;
    }
    Some(PhotoRequestPlan {
        key: key(sel, 1),
        image_id: ImageId(sel.image),
        path: "x.cr3".into(),
        content_hash: "h".into(),
        params: snap.editor.document.params.clone()?,
        viewport_px: VIEWPORT,
        display_px: VIEWPORT,
        window: None,
    })
}

/// One host frame: reconcile → accept → ui_state → (draw) → submit → after_submit.
fn frame(
    view: &mut PhotoView,
    reg: &mut FakeRegistry,
    sel_: Sel,
    snap: &ApplicationSnapshot,
    results: Vec<PhotoResult>,
    retry: bool,
) -> (PhotoUi, Option<PhotoRequest>) {
    view.reconcile(snap);
    view.accept_results(results, reg);
    let ui = view.ui_state(snap);
    reg.in_draw_list = ui.texture.map(|(id, _)| id);
    let request = view.submit(plan(sel_, snap), retry);
    reg.in_draw_list = None;
    view.after_submit(reg);
    (ui, request)
}

fn texture_id(ui: &PhotoUi) -> Option<egui::TextureId> {
    ui.texture.map(|(id, _)| id)
}

#[test]
fn failure_does_not_block_later_photo() {
    let Some(h) = harness() else { return };
    let (a, b) = (sel(1, 1), sel(2, 2));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    let (_, request) = frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    assert_eq!(request.unwrap().key, key(a, 1));
    let failed = PhotoResult::Failed {
        key: key(a, 1),
        message: "decode failed".into(),
    };
    let (ui, _) = frame(&mut view, &mut reg, a, &ready(a), vec![failed], false);
    assert!(ui.failure.is_some(), "results are accepted before ui_state");
    let (ui, request) = frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    assert_eq!(ui.failure.as_deref(), Some("decode failed — img1.cr3"));
    assert!(request.is_none(), "a failed key is not retried on its own");

    let (ui, request) = frame(&mut view, &mut reg, b, &ready(b), vec![], false);
    assert!(ui.failure.is_none());
    assert_eq!(request.unwrap().key, key(b, 1));
    let (ui, _) = frame(
        &mut view,
        &mut reg,
        b,
        &ready(b),
        vec![h.frame(key(b, 1))],
        false,
    );
    assert!(ui.failure.is_none());
    let (ui, _) = frame(&mut view, &mut reg, b, &ready(b), vec![], false);
    assert!(ui.texture.is_some());
    assert!(!ui.loading);
}

#[test]
fn late_failure_of_previous_photo_is_ignored() {
    let Some(_h) = harness() else { return };
    let (a, b) = (sel(1, 1), sel(2, 2));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(&mut view, &mut reg, b, &ready(b), vec![], false);
    let late = PhotoResult::Failed {
        key: key(a, 1),
        message: "late".into(),
    };
    frame(&mut view, &mut reg, b, &ready(b), vec![late], false);
    let (ui, _) = frame(&mut view, &mut reg, b, &ready(b), vec![], false);
    assert!(ui.failure.is_none());
    assert!(ui.loading);
}

#[test]
fn late_frame_of_other_photo_after_returning_is_rejected() {
    let Some(h) = harness() else { return };
    let (a1, b, a2) = (sel(1, 1), sel(2, 2), sel(1, 3));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a1, &ready(a1), vec![], false);
    frame(&mut view, &mut reg, b, &ready(b), vec![], false);
    frame(&mut view, &mut reg, a2, &ready(a2), vec![], false);
    let late_b = h.frame(key(b, 1));
    let (ui, _) = frame(&mut view, &mut reg, a2, &ready(a2), vec![late_b], false);
    assert!(ui.texture.is_none());
    assert_eq!(
        reg.events,
        vec![
            Ev::Register(egui::TextureId::User(1)),
            Ev::Free(egui::TextureId::User(1))
        ]
    );
    frame(
        &mut view,
        &mut reg,
        a2,
        &ready(a2),
        vec![h.frame(key(a2, 1))],
        false,
    );
    let (ui, _) = frame(&mut view, &mut reg, a2, &ready(a2), vec![], false);
    assert!(ui.texture.is_some());
    assert_eq!(h.live(), 1);
}

#[test]
fn loading_document_or_missing_image_row_retires_old_frame() {
    let Some(h) = harness() else { return };
    let (a, b) = (sel(1, 1), sel(2, 2));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(
        &mut view,
        &mut reg,
        a,
        &ready(a),
        vec![h.frame(key(a, 1))],
        false,
    );
    let (ui, _) = frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    let shown = texture_id(&ui).expect("A is shown");

    let loading = snapshot(b, DocumentStatus::Loading);
    let (ui, request) = frame(&mut view, &mut reg, b, &loading, vec![], false);
    assert!(ui.texture.is_none() && ui.loading && request.is_none());
    assert!(reg.freed(shown));
    assert_eq!(h.live(), 0, "old lease returned to the pool");

    let mut no_row = ready(b);
    no_row.editor.image = None;
    let (ui, request) = frame(&mut view, &mut reg, b, &no_row, vec![], false);
    assert!(ui.texture.is_none() && !ui.loading && ui.message.is_none());
    assert!(request.is_none());
}

#[test]
fn corrupt_and_unsupported_documents_show_no_frame_and_no_plan() {
    let Some(h) = harness() else { return };
    let (a, b) = (sel(1, 1), sel(2, 2));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(
        &mut view,
        &mut reg,
        a,
        &ready(a),
        vec![h.frame(key(a, 1))],
        false,
    );
    for status in [
        DocumentStatus::Corrupt,
        DocumentStatus::Unsupported("v99".into()),
    ] {
        let snap = snapshot(b, status);
        let (ui, request) = frame(&mut view, &mut reg, b, &snap, vec![], false);
        assert!(ui.texture.is_none() && ui.message.is_some() && request.is_none());
    }
}

#[test]
fn close_retires_frame_and_late_result_is_stale_then_reopen_renders() {
    let Some(h) = harness() else { return };
    let a = sel(1, 1);
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(
        &mut view,
        &mut reg,
        a,
        &ready(a),
        vec![h.frame(key(a, 1))],
        false,
    );
    let (ui, _) = frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    let shown = texture_id(&ui).unwrap();

    let (ui, request) = frame(&mut view, &mut reg, a, &closed(), vec![], false);
    assert!(ui.texture.is_none() && request.is_none());
    assert!(reg.freed(shown));
    assert_eq!(h.live(), 0);

    let (ui, _) = frame(
        &mut view,
        &mut reg,
        a,
        &closed(),
        vec![h.frame(key(a, 1))],
        false,
    );
    assert!(ui.texture.is_none());
    assert_eq!(h.live(), 0, "late frame rejected and dropped");

    let reopened = Sel { epoch: 2, ..a };
    let (_, request) = frame(
        &mut view,
        &mut reg,
        reopened,
        &ready(reopened),
        vec![],
        false,
    );
    assert_eq!(request.unwrap().key, key(reopened, 1));
    frame(
        &mut view,
        &mut reg,
        reopened,
        &ready(reopened),
        vec![h.frame(key(reopened, 1))],
        false,
    );
    let (ui, _) = frame(
        &mut view,
        &mut reg,
        reopened,
        &ready(reopened),
        vec![],
        false,
    );
    assert!(ui.texture.is_some());
}

#[test]
fn close_with_render_in_flight_rejects_result_and_returns_lease() {
    let Some(h) = harness() else { return };
    let a = sel(1, 1);
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(&mut view, &mut reg, a, &closed(), vec![], false);
    let (ui, _) = frame(
        &mut view,
        &mut reg,
        a,
        &closed(),
        vec![h.frame(key(a, 1))],
        false,
    );
    assert!(ui.texture.is_none());
    assert_eq!(h.live(), 0);
}

#[test]
fn texture_survives_until_after_submit() {
    let Some(h) = harness() else { return };
    let (a, b) = (sel(1, 1), sel(2, 2));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(
        &mut view,
        &mut reg,
        a,
        &ready(a),
        vec![h.frame(key(a, 1))],
        false,
    );

    // Frame in which the UI still draws A while a plan for B arrives (e.g. the plan changed
    // during run_ui): the draw list references A's texture.
    let snap = ready(a);
    view.reconcile(&snap);
    let ui = view.ui_state(&snap);
    let drawn = texture_id(&ui).unwrap();
    reg.in_draw_list = Some(drawn);
    let request = view.submit(plan(b, &ready(b)), false);
    assert!(request.is_some());
    assert!(
        !reg.freed(drawn),
        "freed before the draw list was submitted"
    );
    assert_eq!(h.live(), 1, "lease still checked out until after_submit");
    reg.in_draw_list = None;
    view.after_submit(&mut reg);
    assert!(reg.freed(drawn));
    assert_eq!(h.live(), 0);
}

#[test]
fn retry_resubmits_once_and_only_on_request() {
    let Some(_h) = harness() else { return };
    let a = sel(1, 1);
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    let failed = PhotoResult::Failed {
        key: key(a, 1),
        message: "boom".into(),
    };
    frame(&mut view, &mut reg, a, &ready(a), vec![failed], false);
    let (ui, request) = frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    assert!(ui.failure.is_some() && request.is_none());
    let (_, request) = frame(&mut view, &mut reg, a, &ready(a), vec![], true);
    assert_eq!(request.unwrap().key, key(a, 1));
    let (ui, request) = frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    assert!(ui.failure.is_none() && request.is_none());
}

fn histogram_for(sel: Sel) -> PhotoResult {
    PhotoResult::Histogram {
        key: key(sel, 1),
        r: vec![1; 256],
        g: vec![2; 256],
        b: vec![3; 256],
    }
}

#[test]
fn histogram_of_old_subject_is_never_exposed() {
    let (a, b) = (sel(1, 1), sel(2, 2));
    let mut view = PhotoView::new(DeviceEpoch(1));
    let mut reg = FakeRegistry::default();
    frame(&mut view, &mut reg, a, &ready(a), vec![], false);
    frame(
        &mut view,
        &mut reg,
        a,
        &ready(a),
        vec![histogram_for(a)],
        false,
    );
    assert_eq!(view.histogram(&ready(a)).map(|h| h.image), Some(ImageId(1)));
    // Selection moves on: the old distribution is dropped, and a late one is not stored.
    frame(&mut view, &mut reg, b, &ready(b), vec![], false);
    assert!(view.histogram(&ready(b)).is_none());
    frame(
        &mut view,
        &mut reg,
        b,
        &ready(b),
        vec![histogram_for(a)],
        false,
    );
    assert!(view.histogram(&ready(b)).is_none());
    // The new subject's own histogram is exposed, and never under another snapshot.
    frame(
        &mut view,
        &mut reg,
        b,
        &ready(b),
        vec![histogram_for(b)],
        false,
    );
    assert_eq!(view.histogram(&ready(b)).map(|h| h.image), Some(ImageId(2)));
    assert!(view.histogram(&ready(a)).is_none());
    assert!(view.histogram(&closed()).is_none());
}
