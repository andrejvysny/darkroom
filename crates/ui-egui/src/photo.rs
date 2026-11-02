//! Central photo panel: presentation state ([`PhotoUi`]), the pure render plan the panel
//! derives from the snapshot ([`PhotoRequestPlan`]) and the fit-view geometry helpers.

use core_app::state::{ApplicationSnapshot, DocumentStatus};
use core_model::catalog::ImageRow;
use core_model::identity::{
    CacheVersion, DeviceEpoch, DisplayRevision, EditRevision, FrameKey, ImageId, ProcessVersion,
    QualityTier, RenderIdentity, RendererVersion, SourceIdentity, SourceRevision, ViewportRevision,
};
use core_model::viewport::{derive_view, ViewState};
use core_model::{Crop, DevelopParams};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
};

/// A render plan the host must submit: `key` identifies the frame, the rest is the payload for
/// the host's photo worker, which derives the GPU view parameters from `viewport_px`. Computed
/// by the central-panel draw (the only place the panel's rect is known) and consumed by the
/// host after `run_ui`.
pub struct PhotoRequestPlan {
    pub key: FrameKey,
    pub image_id: ImageId,
    pub path: PathBuf,
    pub content_hash: String,
    pub params: Arc<DevelopParams>,
    /// The size to REQUEST a frame at: the fitted size when settled, half of it during an edit
    /// gesture (`QualityTier::Interactive`). The stage stretches whatever frame it holds to
    /// `display_px`, so both tiers fill the same area.
    pub viewport_px: (u32, u32),
    /// The size the frame is DISPLAYED at: the largest size with the cropped frame's aspect that
    /// fits the panel at fit, or the visible image area when zoomed, independent of the tier.
    pub display_px: (u32, u32),
    /// The crop-local window to render — `((origin u, origin v), (size u, size v))` — or `None`
    /// for the fit view, which renders the whole crop (`ViewParams::full`) exactly as before.
    pub window: Option<([f32; 2], [f32; 2])>,
}

/// Photo presentation state for the central panel, owned by the host (`window.rs`): the egui
/// texture of the presented frame, the status label, and the out-param for this frame's plan.
pub struct PhotoUi {
    /// Registered egui texture of the frame the presenter currently holds, with its pixel size.
    pub texture: Option<(egui::TextureId, [u32; 2])>,
    /// Explicit state label (protected/unsupported/failed). When set it replaces the image —
    /// an unreadable or unsupported document must never be silently shown as a render.
    pub message: Option<String>,
    /// The worker failed to develop the wanted frame. Shown with a Retry button; the plan is
    /// still computed so a later, different request (resize, edit, other photo) is never blocked.
    pub failure: Option<String>,
    /// Out-param: the user pressed Retry on `failure` this frame.
    pub retry: bool,
    /// A frame is wanted but not yet presented (or absent at all).
    pub loading: bool,
    /// Device epoch stamped into every wanted key (the host's renderer epoch).
    pub device_epoch: DeviceEpoch,
    /// Filled by the central-panel draw when a frame should be rendered at the panel size.
    pub plan: Option<PhotoRequestPlan>,
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

/// The interactive-tier render size: half the displayed size per side, each side >= 1 (a 1×1
/// display stays 1×1 — the renderer must never be handed a zero-sized target). Odd sides round
/// down, which is fine: the frame is stretched to the displayed rect anyway.
pub fn interactive_render_px(display: (u32, u32)) -> (u32, u32) {
    ((display.0 / 2).max(1), (display.1 / 2).max(1))
}

/// Aspect (W/H) of what the pipeline outputs for this edit: the CROPPED frame's aspect, from
/// the same crop-aware output dims the legacy develop viewport uses —
/// [`Crop::output_size`] over the image's display dims (incl. rot90; the catalog stores the
/// oriented display dims, which is exactly what `develop_linear` uprights to). Missing/zero
/// catalog dims (legacy rows) fall back to `fallback` (no correction), never a bogus aspect.
pub fn cropped_frame_aspect(
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

/// The CROPPED frame's pixel dims from the catalog's display dims — the same derivation
/// [`cropped_frame_aspect`] uses — or `None` when the catalog carries none (a legacy row has no
/// geometry to zoom against, so the stage keeps the fit view and refuses zoom input).
pub(crate) fn cropped_frame_px(
    crop: &Crop,
    width: Option<i64>,
    height: Option<i64>,
) -> Option<(u32, u32)> {
    let width = u32::try_from(width?).ok()?;
    let height = u32::try_from(height?).ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some(crop.output_size(width, height))
}

/// Stable per-process hash of the render geometry: the render size and, when zoomed, the exact
/// window (its `f32` bit patterns, so a pan is a different frame and never a reused one). With no
/// window this is exactly the historical fit-view revision — fit keys never change.
pub fn viewport_revision(
    width: u32,
    height: u32,
    window: Option<([f32; 2], [f32; 2])>,
) -> ViewportRevision {
    let mut hasher = DefaultHasher::new();
    width.hash(&mut hasher);
    height.hash(&mut hasher);
    if let Some((origin, size)) = window {
        for value in origin.into_iter().chain(size) {
            value.to_bits().hash(&mut hasher);
        }
    }
    ViewportRevision(hasher.finish())
}

/// What to render and display for a view: the displayed size plus the crop-local window the
/// renderer renders (`None` = the whole crop).
struct PlanGeometry {
    display_px: (u32, u32),
    window: Option<([f32; 2], [f32; 2])>,
}

/// At fit (or without catalog dims to zoom against) the whole crop at the fitted size — exactly
/// the historical plan; zoomed, the visible image area and the crop-local window that is a 1:1
/// picture of it.
fn plan_geometry(
    crop: &Crop,
    width: Option<i64>,
    height: Option<i64>,
    panel_px: (u32, u32),
    aspect: f64,
    view: ViewState,
) -> PlanGeometry {
    let zoomed = cropped_frame_px(crop, width, height)
        .filter(|_| !view.is_fit())
        .map(|frame_px| derive_view(view, frame_px, panel_px));
    match zoomed {
        Some(derived) => PlanGeometry {
            display_px: derived.out_px,
            window: Some((
                [derived.origin[0] as f32, derived.origin[1] as f32],
                [derived.size[0] as f32, derived.size[1] as f32],
            )),
        },
        None => PlanGeometry {
            display_px: fit_output(panel_px, aspect),
            window: None,
        },
    }
}

/// The wanted frame for a `Ready` document at this panel rect under `view`, or `None` when
/// nothing is renderable (no selection, document not `Ready`). Protection (denoise/AI masks,
/// corrupt, unreadable) is enforced by the host through `PhotoUi::message` + the worker's own
/// check; a plan is only ever produced for params the worker accepts.
///
/// While an edit gesture is in flight (`gesture_active`) or the user is zooming/panning
/// (`interacting`) the frame is thrown away by the next event anyway, so it is asked for at half
/// the displayed size and the `Interactive` tier: four times less GPU work per update, and the
/// worker skips the histogram for that tier. The stage stretches the frame it holds to the
/// displayed rect, so the picture still fills the panel. Ending the gesture changes the render
/// size and the tier, which is a new key: the full-size, histogram-bearing `Settled` frame
/// follows immediately.
///
/// At fit (`view.is_fit()`) or without catalog dims the plan is exactly the historical one: the
/// whole crop rendered at the fitted size, `window: None`. Zoomed, only the visible window of the
/// crop is rendered, at the physical size of the visible image area.
#[allow(clippy::too_many_arguments)]
pub(crate) fn photo_plan(
    snapshot: &ApplicationSnapshot,
    rect: egui::Rect,
    ppp: f32,
    device_epoch: DeviceEpoch,
    view: ViewState,
    interacting: bool,
) -> Option<PhotoRequestPlan> {
    let image = snapshot.editor.image.as_ref()?;
    if snapshot.editor.document.status != DocumentStatus::Ready {
        return None;
    }
    let params = snapshot.editor.document.params.clone()?;
    let revision = snapshot.editor.document.revision?;
    let process_version = snapshot.editor.document.process_version?;
    let panel_w = (rect.width() * ppp).round().max(1.0) as u32;
    let panel_h = (rect.height() * ppp).round().max(1.0) as u32;
    // Aspect-correct fit view: the displayed frame is the largest size with the CROPPED frame's
    // aspect that fits the panel — drawing it 1:1 centered never stretches it to the panel.
    let aspect = cropped_frame_aspect(
        &params.crop,
        image.width,
        image.height,
        f64::from(panel_w) / f64::from(panel_h),
    );
    let PlanGeometry { display_px, window } = plan_geometry(
        &params.crop,
        image.width,
        image.height,
        (panel_w, panel_h),
        aspect,
        view,
    );
    let (viewport_px, quality) = if snapshot.editor.document.gesture_active || interacting {
        (interactive_render_px(display_px), QualityTier::Interactive)
    } else {
        (display_px, QualityTier::Settled)
    };
    let key = wanted_key(
        snapshot,
        image,
        revision,
        process_version,
        viewport_px,
        quality,
        ppp,
        device_epoch,
        window,
    );
    Some(PhotoRequestPlan {
        key,
        image_id: ImageId(image.id),
        path: PathBuf::from(&image.path),
        content_hash: image.content_hash.clone(),
        params,
        viewport_px,
        display_px,
        window,
    })
}

/// The identity of the frame a plan wants. The content hash rides in the request (the
/// prepared-cache key) and pins the identity's source revision, so a same-id re-index with new
/// bytes is a different frame — otherwise `PhotoView::submit` would keep the stale frame (its
/// wanted key never changes). `viewport_px`/`quality` and the zoom `window` are the RENDER
/// geometry, so a tier, render-size or window change is always a new key, never a silently
/// reused frame.
#[allow(clippy::too_many_arguments)]
fn wanted_key(
    snapshot: &ApplicationSnapshot,
    image: &ImageRow,
    revision: EditRevision,
    process_version: ProcessVersion,
    viewport_px: (u32, u32),
    quality: QualityTier,
    ppp: f32,
    device_epoch: DeviceEpoch,
    window: Option<([f32; 2], [f32; 2])>,
) -> FrameKey {
    FrameKey {
        identity: RenderIdentity {
            source: SourceIdentity {
                catalog_epoch: snapshot.catalog_epoch,
                image_id: ImageId(image.id),
                source_revision: SourceRevision::from_content_hash(&image.content_hash),
            },
            editor_session: snapshot.editor.editor_session,
            selection_generation: snapshot.editor.selection_generation,
            edit_revision: revision,
            process_version,
            renderer_version: RendererVersion(1),
            cache_version: CacheVersion(1),
            viewport_revision: viewport_revision(viewport_px.0, viewport_px.1, window),
            display_revision: DisplayRevision(0),
            device_epoch,
        },
        viewport_px,
        scale_milli: (ppp * 1000.0).round() as u32,
        quality,
    }
}

/// Unit tests for the fit geometry and the plan (they read this module's private items).
#[cfg(test)]
mod plan_tests;
