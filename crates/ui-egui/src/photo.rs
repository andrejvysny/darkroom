//! Central photo panel: presentation state ([`PhotoUi`]), the pure render plan the panel
//! derives from the snapshot ([`PhotoRequestPlan`]) and the fit-view geometry helpers.

use core_app::state::{ApplicationSnapshot, DocumentStatus};
use core_model::identity::{
    CacheVersion, DeviceEpoch, DisplayRevision, FrameKey, ImageId, QualityTier, RenderIdentity,
    RendererVersion, SourceIdentity, SourceRevision, ViewportRevision,
};
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
    pub viewport_px: (u32, u32),
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

/// Stable per-process hash of the viewport pixel size (fit view: no zoom yet, so the geometry
/// revision depends on nothing else).
pub fn viewport_revision(width: u32, height: u32) -> ViewportRevision {
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
    Some(PhotoRequestPlan {
        key,
        image_id: ImageId(image.id),
        path: PathBuf::from(&image.path),
        content_hash: image.content_hash.clone(),
        params,
        viewport_px: (width, height),
    })
}

/// The central panel: error label, Retry strip, the presented frame (or its state label) and
/// the out-param plan for this frame's panel rect.
pub(crate) fn central_panel(
    root: &mut egui::Ui,
    snapshot: &ApplicationSnapshot,
    photo: &mut PhotoUi,
) {
    egui::CentralPanel::default().show(root, |ui| {
        // Save failures have their own banner with Discard/Dismiss (see `develop`).
        if let Some(error) = snapshot
            .error
            .as_ref()
            .filter(|error| !crate::develop::is_save_error(error))
        {
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
        if let Some(failure) = &photo.failure {
            ui.horizontal(|ui| {
                ui.label(failure);
                if ui.button("Retry").clicked() {
                    photo.retry = true;
                }
            });
        }
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
}

#[cfg(test)]
mod tests {
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
