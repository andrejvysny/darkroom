//! Unit tests for the central panel's fit geometry ([`super::fit_output`],
//! [`super::cropped_frame_aspect`]) and the render plan ([`super::photo_plan`]): the window,
//! the interactive tier and the viewport revision a zoomed view asks the worker for.

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
    assert!((cropped_frame_aspect(&rot, Some(6960), Some(4640), -1.0) - 2.0 / 3.0).abs() < 1e-12);
    // Missing/zero catalog dims fall back to the caller's aspect, never 0/NaN.
    assert_eq!(cropped_frame_aspect(&full, None, Some(4640), 1.25), 1.25);
    assert_eq!(cropped_frame_aspect(&full, Some(0), Some(4640), 1.25), 1.25);
}

use super::{
    interactive_render_px, photo_plan, viewport_revision, DevelopParams, PhotoRequestPlan,
    QualityTier, StageVariant,
};
use core_app::state::{
    ApplicationSnapshot, DocumentStatus, DocumentView, EditorSnapshot, JobsSnapshot,
    LibrarySnapshot, SettingsSnapshot,
};
use core_model::catalog::ImageRow;
use core_model::identity::{
    DeviceEpoch, EditRevision, ProcessVersion, SourceRevision, ViewportRevision,
};
use core_model::viewport::{derive_view, ViewState};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const HASH: &str = "00000000000000ff0123456789abcdef";

/// A `Ready` editor holding `content_hash`, mid-gesture or not; the frame key must follow
/// the hash and the tier must follow the gesture.
fn ready_with(hash: &str, gesture_active: bool) -> ApplicationSnapshot {
    ApplicationSnapshot {
        revision: 1,
        running: true,
        catalog_ready: true,
        catalog_epoch: Default::default(),
        library: LibrarySnapshot::default(),
        jobs: JobsSnapshot::default(),
        settings: SettingsSnapshot::default(),
        error: None,
        editor: EditorSnapshot {
            selected: Some(core_model::identity::ImageId(1)),
            image: Some(ImageRow {
                id: 1,
                content_hash: hash.to_owned(),
                width: Some(6960),
                height: Some(4640),
                ..Default::default()
            }),
            document: DocumentView {
                status: DocumentStatus::Ready,
                params: Some(Arc::new(DevelopParams::default())),
                revision: Some(EditRevision(1)),
                process_version: Some(ProcessVersion(6)),
                gesture_active,
                ..Default::default()
            },
            ..Default::default()
        },
    }
}

/// The test stage: 820×625 points at ppp 1, i.e. an 820×625 physical-px panel.
fn panel() -> egui::Rect {
    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(820.0, 625.0))
}

/// The fixture's cropped frame: the catalog dims with a full crop.
const FRAME_PX: (u32, u32) = (6960, 4640);

fn planned(
    hash: &str,
    gesture_active: bool,
    view: ViewState,
    interacting: bool,
) -> PhotoRequestPlan {
    photo_plan(
        &ready_with(hash, gesture_active),
        panel(),
        1.0,
        DeviceEpoch(1),
        view,
        interacting,
        StageVariant::Edited,
    )
    .expect("a Ready document with params plans a frame")
}

fn plan_at(hash: &str, gesture_active: bool) -> PhotoRequestPlan {
    planned(hash, gesture_active, ViewState::default(), false)
}

fn plan(hash: &str) -> PhotoRequestPlan {
    plan_at(hash, false)
}

#[test]
fn a_reindexed_image_gets_a_new_frame_key() {
    // The DARKROOM-162 defect: same id, same edit revision, new bytes must not reuse the
    // presented frame — the content hash has to reach the identity's source revision.
    let before = plan(HASH);
    let after = plan("00000000000000fe0123456789abcdef");
    // The pre-fix key pinned 0 here, which is what made the frame unreplaceable.
    assert_ne!(
        before.key.identity.source.source_revision,
        SourceRevision(0)
    );
    assert_ne!(before.key, after.key);
    assert_eq!(before.image_id, after.image_id);
    assert_eq!(before.viewport_px, after.viewport_px);
    // The new bytes are the ONLY difference: restoring the old revision restores the key.
    let mut replayed = after.key;
    replayed.identity.source.source_revision = before.key.identity.source.source_revision;
    assert_eq!(replayed, before.key);
}

#[test]
fn the_same_hash_always_plans_the_same_key() {
    assert_eq!(plan(HASH).key, plan(HASH).key);
}

#[test]
fn interactive_render_px_halves_each_side_without_ever_reaching_zero() {
    assert_eq!(interactive_render_px((820, 625)), (410, 312));
    // Odd sides round down; the frame is stretched to the displayed rect anyway.
    assert_eq!(interactive_render_px((411, 313)), (205, 156));
    assert_eq!(interactive_render_px((1, 1)), (1, 1));
    assert_eq!(interactive_render_px((0, 3)), (1, 1));
}

#[test]
fn a_settled_plan_renders_at_the_displayed_size() {
    let settled = plan(HASH);
    assert_eq!(settled.key.quality, QualityTier::Settled);
    assert_eq!(settled.viewport_px, settled.display_px);
}

#[test]
fn a_gesture_plan_renders_half_size_at_the_interactive_tier() {
    // The drag cost (DARKROOM-163): every update used to develop a full-size frame plus a
    // histogram, so the worker was never free and the drag stuttered.
    let settled = plan(HASH);
    let drag = plan_at(HASH, true);
    assert_eq!(drag.key.quality, QualityTier::Interactive);
    // Same displayed area: the stage stretches the half-size frame to the settled rect.
    assert_eq!(drag.display_px, settled.display_px);
    assert_eq!(drag.viewport_px, interactive_render_px(settled.display_px));
    assert_ne!(drag.viewport_px, settled.viewport_px);
    // A different tier and render size must be a different key, or `submit` would keep the
    // settled frame and the drag would never land.
    assert_ne!(drag.key, settled.key);
    let mut same_but_geometry = drag.key.identity;
    same_but_geometry.viewport_revision = settled.key.identity.viewport_revision;
    assert_eq!(
        same_but_geometry, settled.key.identity,
        "only the render geometry/tier may differ"
    );
}

#[test]
fn an_interacting_view_asks_for_the_interactive_tier_without_a_gesture() {
    // Zoom/pan is its own gesture: every update during one is thrown away by the next.
    let settled = planned(HASH, false, ViewState::default(), false);
    let panning = planned(HASH, false, ViewState::default(), true);
    assert_eq!(panning.key.quality, QualityTier::Interactive);
    assert_eq!(panning.display_px, settled.display_px);
    assert_ne!(panning.key, settled.key);
}

#[test]
fn the_fit_view_plans_no_window_and_the_size_it_always_did() {
    let fit = plan(HASH);
    assert_eq!(fit.window, None);
    assert_eq!(fit.display_px, fit.viewport_px);
    assert_eq!(fit.key.quality, QualityTier::Settled);
}

#[test]
fn a_windowless_revision_is_the_historical_size_hash() {
    // Fit keys must not change with zoom existing: no window ⇒ exactly the old two-field hash.
    let mut hasher = DefaultHasher::new();
    820u32.hash(&mut hasher);
    625u32.hash(&mut hasher);
    assert_eq!(
        viewport_revision(820, 625, None),
        ViewportRevision(hasher.finish())
    );
}

#[test]
fn a_zoomed_view_renders_only_the_visible_window() {
    let fit = plan(HASH);
    let view = ViewState {
        zoom: 2.0,
        center: [0.5, 0.5],
    };
    let zoomed = planned(HASH, false, view, false);
    let derived = derive_view(view, FRAME_PX, (820, 625));
    let (origin, size) = zoomed.window.expect("zoomed plans a window");
    // The plan carries the derived window at `f32` precision (`ViewParams` is `f32`).
    for axis in 0..2 {
        assert!(
            (f64::from(origin[axis]) - derived.origin[axis]).abs() <= 1e-6,
            "origin {origin:?} vs {:?}",
            derived.origin
        );
        assert!(
            (f64::from(size[axis]) - derived.size[axis]).abs() <= 1e-6,
            "size {size:?} vs {:?}",
            derived.size
        );
    }
    assert!(
        size[0] < 1.0 && size[1] < 1.0 && origin[0] > 0.0 && origin[1] > 0.0,
        "{origin:?} {size:?} is not a sub-window of the crop"
    );
    // Panel-limited: at 2× the visible image area is the whole panel, not the fit box.
    assert_eq!(zoomed.display_px, derived.out_px);
    assert_eq!(zoomed.display_px, (820, 625));
    assert_eq!(zoomed.viewport_px, zoomed.display_px);
    assert_ne!(zoomed.display_px, fit.display_px);
    assert_ne!(zoomed.key, fit.key);
}

#[test]
fn panning_changes_the_key_although_the_render_size_stays() {
    // Same size, moved window: reusing the presented frame here would freeze the picture.
    let base = ViewState {
        zoom: 2.0,
        center: [0.45, 0.5],
    };
    let panned = ViewState {
        zoom: 2.0,
        center: [0.55, 0.5],
    };
    let before = planned(HASH, false, base, false);
    let after = planned(HASH, false, panned, false);
    assert_eq!(before.display_px, after.display_px);
    assert_eq!(before.viewport_px, after.viewport_px);
    assert_ne!(before.window, after.window);
    assert_ne!(before.key, after.key);
}

#[test]
fn a_photo_without_catalog_dims_never_plans_a_window() {
    // No geometry to zoom against: the plan stays the fit view however zoomed the view is.
    let mut snap = ready_with(HASH, false);
    snap.editor.image.as_mut().expect("image").width = None;
    let plan = photo_plan(
        &snap,
        panel(),
        1.0,
        DeviceEpoch(1),
        ViewState {
            zoom: 4.0,
            center: [0.3, 0.7],
        },
        false,
        StageVariant::Edited,
    )
    .expect("a Ready document with params plans a frame");
    assert_eq!(plan.window, None);
    assert_eq!(plan.display_px, plan.viewport_px);
}

fn planned_variant(
    snap: &ApplicationSnapshot,
    view: ViewState,
    variant: StageVariant,
) -> PhotoRequestPlan {
    photo_plan(snap, panel(), 1.0, DeviceEpoch(1), view, false, variant)
        .expect("a Ready document with params plans a frame")
}

fn edited_snapshot() -> ApplicationSnapshot {
    let mut snap = ready_with(HASH, false);
    snap.editor.document.params = Some(Arc::new(DevelopParams {
        exposure: 1.5,
        crop: Crop {
            hw: 0.25,
            angle: 3.0,
            rot90: 1,
            ..Crop::default()
        },
        ..Default::default()
    }));
    snap
}

#[test]
fn the_edited_variant_is_the_historical_plan() {
    let snap = edited_snapshot();
    let edited = planned_variant(&snap, ViewState::default(), StageVariant::Edited);
    assert_eq!(edited.params, snap.editor.document.params.clone().unwrap());
    assert!(!edited.crop_preview);
    assert_eq!(
        edited.key.identity.viewport_revision,
        viewport_revision(edited.viewport_px.0, edited.viewport_px.1, None),
        "Edited keys must hash exactly the historical two fields"
    );
}

#[test]
fn the_before_plan_uses_default_params_under_its_own_key() {
    let snap = edited_snapshot();
    let edited = planned_variant(&snap, ViewState::default(), StageVariant::Edited);
    let before = planned_variant(&snap, ViewState::default(), StageVariant::Before);
    assert_eq!(*before.params, DevelopParams::default());
    assert!(!before.crop_preview);
    assert_ne!(before.key, edited.key);
    // Same geometry class: the identity differs only by the viewport revision.
    let mut same = before.key.identity;
    same.viewport_revision = edited.key.identity.viewport_revision;
    assert_eq!(same.edit_revision, edited.key.identity.edit_revision);
    // The default crop is the full, unrotated frame: its aspect is the catalog's.
    assert_eq!(before.display_px, (820, 547));
}

#[test]
fn the_before_plan_keeps_the_sources_display_referred_flag() {
    let mut snap = ready_with(HASH, false);
    snap.editor.document.params = Some(Arc::new(DevelopParams {
        display_referred: true,
        exposure: 1.0,
        ..Default::default()
    }));
    let before = planned_variant(&snap, ViewState::default(), StageVariant::Before);
    assert_eq!(
        *before.params,
        DevelopParams {
            display_referred: true,
            ..Default::default()
        }
    );
}

#[test]
fn the_crop_tool_plan_resets_the_crop_but_keeps_rotation_and_ignores_zoom() {
    let snap = edited_snapshot();
    let zoomed = ViewState {
        zoom: 3.0,
        center: [0.3, 0.6],
    };
    let tool = planned_variant(&snap, zoomed, StageVariant::CropTool);
    assert!(tool.crop_preview);
    assert_eq!(
        tool.window, None,
        "the crop tool always renders the fit view"
    );
    assert_eq!(
        tool.params.crop,
        Crop {
            angle: 3.0,
            rot90: 1,
            ..Crop::default()
        }
    );
    assert_eq!(tool.params.exposure, 1.5, "only the crop is reset");
    // rot90 = 1 swaps the full frame to 4640x6960 portrait.
    assert_eq!(
        tool.display_px,
        planned_variant(&snap, ViewState::default(), StageVariant::CropTool).display_px
    );
    assert!(tool.display_px.0 < tool.display_px.1);
    let edited = planned_variant(&snap, ViewState::default(), StageVariant::Edited);
    assert_ne!(tool.key, edited.key);
    assert_ne!(
        tool.key,
        planned_variant(&snap, ViewState::default(), StageVariant::Before).key
    );
}
