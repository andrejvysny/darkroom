//! Panorama registration core (Phase P1).
//!
//! Pipeline: features → matching → RANSAC → match graph → camera model → bundle adjustment → wave
//! correction. This crate is pure Rust and deliberately does NOT link rawler — it consumes plain f32
//! camera-native RGB [`Frame`]s (produced upstream by `core_raw::develop_camera_native`) and returns
//! a [`Registration`] describing where each frame sits on the panorama sphere.
//!
//! Compositing (warp / multi-band blend / auto-crop / rectangling / encode) is Phase P2. It is not
//! implemented here, but the public surface is shaped for it: [`Registration`] carries everything a
//! compositor needs (per-frame [`CameraPose`], the used set, the reference frame, and the verified
//! [`PairMatch`]es with their inliers for gain/seam work), and the [`Phase`], [`Projection`], and
//! [`StitchOptions`] types are already defined so `stitch()` can slot in without touching P1.
//!
//! ## Coordinate & convention notes
//! - Features are detected at registration scale (~900 px long side) but every keypoint is stored in
//!   FULL-RES pixels, so homographies, focals, and principal points are in the camera's real pixel
//!   units. RANSAC's 3 px threshold is specified at registration scale and scaled to full-res by
//!   `1 / reg_scale` inside [`ransac`].
//! - Homography direction: [`PairMatch::h`] maps image-`i` points to image-`j` points (`q ≈ H·p`).
//! - Luma for features is `Y = 0.299R + 0.587G + 0.114B` on the linear data, then percentile-stretched
//!   (2%..98%) to u8 because camera-native values are dim; see [`features`].
//! - Memory: [`stitch`] / [`register`] / [`compose`] sample resident [`Frame`]s. [`stitch_streaming`]
//!   and [`register_streaming`] take a [`FrameSource`] instead and hold at most ONE full-res frame at
//!   a time — every frame is materialized once downscaled (registration, exposure gains, seams) and
//!   then once at full resolution as it is blended. Because the poses are always expressed in full-res
//!   units, the two paths are interchangeable.
//! - Cancellation: only the streaming entry points take a `cancel` probe; it is polled between source
//!   loads, inside the parallel feature/pair sweeps, and between warped frames, returning
//!   [`PanoError::Cancelled`]. The non-streaming entry points are their `cancel`-free wrappers.

pub mod align;
mod blend;
mod bundle;
mod camera;
mod crop;
mod detect;
mod exposure;
mod features;
mod graph;
mod matching;
mod project;
mod ransac;
mod rectangle;
mod rng;
mod seam;
mod wave;

pub use align::{estimate_alignment_rgb, AlignModel};
pub use detect::{
    detect_groups, DetectOptions, DetectReport, DetectedGroup, EdgeClass, VerifiedEdge,
};

use nalgebra::{Matrix3, Point2};
use rayon::prelude::*;

/// One input frame: camera-native linear RGB, interleaved row-major (`rgb.len() == width*height*3`).
///
/// `focal_seed_px` is an optional focal prior in full-res pixels (e.g. from EXIF); it is only used as
/// a fallback when the homographies fail to yield a focal estimate.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<f32>,
    pub focal_seed_px: Option<f32>,
}

/// One frame materialized by a [`FrameSource`], possibly at a reduced resolution.
///
/// `width`/`height` describe the buffer actually handed over; `full_width`/`full_height` describe the
/// frame's TRUE full-res geometry. Everything the registration solves (keypoints, homographies,
/// focals, principal points) lives in full-res units regardless of how big the delivered buffer is,
/// so poses from a downscaled pass are directly usable to warp the full-res original.
pub struct LoadedFrame {
    pub rgb: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub full_width: usize,
    pub full_height: usize,
    pub focal_seed_px: Option<f32>,
}

/// A lazily-materialized set of input frames.
///
/// This is the seam that lets [`stitch_streaming`] hold at most ONE full-res source in memory at a
/// time: the stitcher asks for every frame once at a small `max_long_side` (registration, exposure
/// gains and seam finding all run there), then asks for each used frame once more at full resolution
/// during the blend, dropping it before requesting the next. A 10-frame 32 MP merge therefore costs
/// one ~0.4 GB decode plus the small buffers, not ten.
///
/// Implementations must be cheap to call concurrently only in the sense of `Sync` — the stitcher
/// loads sequentially.
pub trait FrameSource: Sync {
    /// Number of input frames.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Materialize frame `i`. When `max_long_side` is `Some(m)` the returned buffer SHOULD have its
    /// long side reduced to ≤ `m` (area-downscaled), but a source may return it larger — the caller
    /// reads the real dimensions off the [`LoadedFrame`]. `full_width`/`full_height` must always
    /// report the untouched geometry.
    fn load(&self, i: usize, max_long_side: Option<u32>) -> Result<LoadedFrame, PanoError>;
}

/// Resident frames as a [`FrameSource`]: `load` copies (and, if asked, area-downscales) the buffer
/// that is already in memory. Used by the tests/examples and by anything that already holds the
/// pixels; the streaming win only materializes for a source that decodes on demand.
impl FrameSource for &[Frame] {
    fn len(&self) -> usize {
        <[Frame]>::len(self)
    }

    fn load(&self, i: usize, max_long_side: Option<u32>) -> Result<LoadedFrame, PanoError> {
        let f = self
            .get(i)
            .ok_or_else(|| PanoError::Load(format!("frame index {i} out of range")))?;
        let (dw, dh) = match max_long_side {
            Some(m) => features::capped_dims(f.width, f.height, m),
            None => (f.width, f.height),
        };
        Ok(LoadedFrame {
            rgb: features::downscale_rgb(&f.rgb, f.width, f.height, dw, dh),
            width: dw,
            height: dh,
            full_width: f.width,
            full_height: f.height,
            focal_seed_px: f.focal_seed_px,
        })
    }
}

/// Output projection surface. P1 records the request; P2 acts on it (`Auto` picks per field of view).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Projection {
    Auto,
    Spherical,
    Cylindrical,
    Perspective,
}

/// Compositing options (consumed by P2's `stitch()`; carried through P1 unchanged).
pub struct StitchOptions {
    pub projection: Projection,
    pub boundary_warp: f32,
    pub auto_crop: bool,
    pub max_long_side: u32,
    pub preview: bool,
}

/// Pipeline phase, reported through the progress callback. P1 emits `Register` and `BundleAdjust`;
/// the remainder are P2.
#[derive(Clone, Copy, Debug)]
pub enum Phase {
    Register,
    BundleAdjust,
    Warp,
    Blend,
    Crop,
    Rectangle,
    Encode,
}

/// A solved camera: focal (full-res px), 3×3 rotation on the panorama sphere, and principal point.
pub struct CameraPose {
    pub focal_px: f64,
    pub rotation: Matrix3<f64>,
    pub ppx: f64,
    pub ppy: f64,
}

/// Registration result — P2's compositor consumes this.
pub struct Registration {
    /// Index-aligned with the input frames; `None` = frame not in the panorama.
    pub cameras: Vec<Option<CameraPose>>,
    /// Largest connected component (frame indices), length ≥ 2.
    pub used_indices: Vec<usize>,
    /// Central frame (max total inliers); its rotation is identity-ish (exactly identity before wave
    /// correction, then rotated with everything else).
    pub reference_index: usize,
    /// Verified pairs (for diagnostics + P2 gain/seam).
    pub pairwise: Vec<PairMatch>,
    /// Final bundle-adjustment RMS (unweighted ray-difference units, radians-ish).
    pub ba_final_rms: f64,
    /// False if BA diverged and the spanning-tree seed poses were kept instead.
    pub ba_converged: bool,
}

/// The stitched panorama (Phase P2 output).
pub struct StitchResult {
    pub width: usize,
    pub height: usize,
    /// Interleaved RGB, camera-native linear, clamped `>= 0` (`rgb.len() == width*height*3`).
    pub rgb: Vec<f32>,
    /// `1` = covered by at least one frame, `0` = empty (`valid_mask.len() == width*height`).
    pub valid_mask: Vec<u8>,
    pub reference_index: usize,
    pub used_indices: Vec<usize>,
    /// The projection actually used — never [`Projection::Auto`] (resolved from FOV / overrides).
    pub projection_used: Projection,
    /// True if [`StitchOptions::max_long_side`] forced a uniform downscale of the canvas.
    pub capped: bool,
}

/// A verified overlapping pair and its inlier correspondences.
pub struct PairMatch {
    pub i: usize,
    pub j: usize,
    /// Homography mapping image-`i` points to image-`j` points (`q ≈ H·p`), full-res pixels.
    pub h: Matrix3<f64>,
    pub n_inliers: usize,
    /// Inlier correspondences as `(p_in_i, q_in_j)` in full-res pixels.
    pub inliers: Vec<(Point2<f64>, Point2<f64>)>,
}

#[derive(thiserror::Error, Debug)]
pub enum PanoError {
    #[error("need at least 2 overlapping images, {0} matched")]
    TooFewMatched(usize),
    #[error("cancelled")]
    Cancelled,
    /// A [`FrameSource`] could not materialize a frame (decode failure, missing file, …).
    #[error("{0}")]
    Load(String),
}

/// The cancel probe the non-cancellable public entry points pass through (`&never_cancel`).
fn never_cancel() -> bool {
    false
}

/// Inlier subsample cap per pair for bundle adjustment (keeps BA fast; more is redundant).
const BA_MAX_INLIERS_PER_PAIR: usize = 300;

/// Register a set of frames into a panorama. Runs features → matching → RANSAC → graph → camera model
/// → bundle adjustment → wave correction, and returns per-frame poses plus diagnostics.
///
/// `progress` is invoked at the start of each phase (`Register`, then `BundleAdjust`).
pub fn register(
    frames: &[Frame],
    progress: &(dyn Fn(Phase) + Sync),
) -> Result<Registration, PanoError> {
    let inputs: Vec<RegInput<'_>> = frames.iter().map(RegInput::resident).collect();
    register_inner(&inputs, progress, &never_cancel)
}

/// Register the frames of a [`FrameSource`] without ever holding a full-res buffer: every frame is
/// materialized once at the ~1400 px seam resolution and the keypoints are lifted to full-res units,
/// so the returned poses are in the same units [`register`] would produce.
pub fn register_streaming(
    src: &dyn FrameSource,
    progress: &(dyn Fn(Phase) + Sync),
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<Registration, PanoError> {
    progress(Phase::Register);
    let lows = load_low_pass(src, cancel)?;
    let inputs: Vec<RegInput<'_>> = lows.iter().map(RegInput::loaded).collect();
    register_inner(&inputs, progress, cancel)
}

/// One frame as the registration sees it: a buffer to detect in, plus the TRUE full-res geometry the
/// stored keypoints / principal point / focal seed belong to.
struct RegInput<'a> {
    rgb: &'a [f32],
    width: usize,
    height: usize,
    full_width: usize,
    full_height: usize,
    focal_seed_px: Option<f32>,
}

impl<'a> RegInput<'a> {
    /// A resident frame — the buffer IS the full-res image (the historical `register` path).
    fn resident(f: &'a Frame) -> RegInput<'a> {
        RegInput {
            rgb: &f.rgb,
            width: f.width,
            height: f.height,
            full_width: f.width,
            full_height: f.height,
            focal_seed_px: f.focal_seed_px,
        }
    }

    fn loaded(f: &'a LoadedFrame) -> RegInput<'a> {
        RegInput {
            rgb: &f.rgb,
            width: f.width,
            height: f.height,
            full_width: f.full_width,
            full_height: f.full_height,
            focal_seed_px: f.focal_seed_px,
        }
    }
}

/// Shared body of [`register`] / [`register_streaming`]. With `full == buffer` dims and a
/// never-firing `cancel` this is bit-for-bit the pre-streaming pipeline.
fn register_inner(
    frames: &[RegInput<'_>],
    progress: &(dyn Fn(Phase) + Sync),
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<Registration, PanoError> {
    progress(Phase::Register);

    let n = frames.len();
    if n < 2 {
        return Err(PanoError::TooFewMatched(n));
    }

    // --- Features (parallel over frames) ---
    // `None` marks a frame skipped because cancellation fired mid-sweep; any `None` aborts below.
    let pattern = features::fixed_test_pairs();
    let feats: Vec<Option<features::FrameFeatures>> = frames
        .par_iter()
        .map(|f| {
            if cancel() {
                return None;
            }
            Some(features::extract_at(
                f.rgb,
                f.width,
                f.height,
                f.full_width,
                f.full_height,
                &pattern,
            ))
        })
        .collect();
    if feats.iter().any(|f| f.is_none()) {
        return Err(PanoError::Cancelled);
    }
    let feats: Vec<features::FrameFeatures> = feats.into_iter().flatten().collect();

    // --- Matching + RANSAC + verification (parallel over unordered pairs) ---
    let pair_list: Vec<(usize, usize)> = (0..n)
        .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
        .collect();

    let pairwise: Vec<PairMatch> = pair_list
        .par_iter()
        .filter_map(|&(i, j)| {
            if cancel() {
                return None;
            }
            let matches = matching::match_descriptors(&feats[i].descriptors, &feats[j].descriptors);
            if matches.len() < 4 {
                return None;
            }
            let corr: Vec<(Point2<f64>, Point2<f64>)> = matches
                .iter()
                .map(|&(a, b)| {
                    let (px, py) = feats[i].points[a];
                    let (qx, qy) = feats[j].points[b];
                    (Point2::new(px, py), Point2::new(qx, qy))
                })
                .collect();
            let reg_scale = feats[i].reg_scale.min(feats[j].reg_scale);
            let pair_index = (i * n + j) as u64;
            ransac::verify_pair(&corr, reg_scale, pair_index).map(|v| PairMatch {
                i,
                j,
                h: v.h,
                n_inliers: v.n_inliers,
                inliers: v.inliers,
            })
        })
        .collect();
    // A cancelled sweep silently drops pairs, so never let a partial `pairwise` reach the graph.
    if cancel() {
        return Err(PanoError::Cancelled);
    }

    // --- Match graph: largest component + max-inlier spanning tree ---
    let edges: Vec<graph::Edge> = pairwise
        .iter()
        .enumerate()
        .map(|(idx, pm)| graph::Edge {
            a: pm.i,
            b: pm.j,
            weight: pm.n_inliers,
            pair_idx: idx,
        })
        .collect();
    let tree = graph::largest_component_tree(n, &edges)?;

    progress(Phase::BundleAdjust);

    // --- Camera model: shared focal + spanning-tree rotation seeds ---
    // Principal points (like every keypoint) are in FULL-RES pixels, even when the buffers we just
    // detected in were downscaled.
    let principal_points: Vec<(f64, f64)> = frames
        .iter()
        .map(|f| (f.full_width as f64 * 0.5, f.full_height as f64 * 0.5))
        .collect();

    // Focal from every verified homography; fall back to the reference frame's seed / a wide guess.
    let homs: Vec<Matrix3<f64>> = pairwise.iter().map(|pm| pm.h).collect();
    let focal = camera::estimate_focal(
        &homs,
        frames[tree.reference].full_width as f64,
        frames[tree.reference].focal_seed_px,
    );

    let pairwise_ijh: Vec<(usize, usize, Matrix3<f64>)> =
        pairwise.iter().map(|pm| (pm.i, pm.j, pm.h)).collect();
    let seeds_all = camera::seed_cameras(&tree, &pairwise_ijh, focal, &principal_points);

    // Seed cameras in `used` order.
    let seed_cams: Vec<camera::CameraInit> = tree
        .used
        .iter()
        .map(|&f| seeds_all[f].clone().expect("used frame must be seeded"))
        .collect();

    // --- Bundle adjustment ---
    // Local (used-order) index per frame.
    let mut local_of = vec![usize::MAX; n];
    for (local, &f) in tree.used.iter().enumerate() {
        local_of[f] = local;
    }
    let obs = build_observations(&pairwise, &tree.used, &local_of);

    let ba = bundle::bundle_adjust(&tree.used, tree.reference, &seed_cams, &obs);

    // --- Wave correction (after BA), on the used rotations in `used` order ---
    // wave.rs is a faithful OpenCV port and expects camera→world rotations (its moment matrix is
    // built from camera axes = matrix columns). Our poses are world→camera, so transpose in and back.
    let mut cam_to_world: Vec<Matrix3<f64>> = ba.rotations.iter().map(|r| r.transpose()).collect();
    wave::wave_correct_horiz(&mut cam_to_world);
    let rotations: Vec<Matrix3<f64>> = cam_to_world.iter().map(|r| r.transpose()).collect();

    // --- Assemble per-frame poses ---
    let mut cameras: Vec<Option<CameraPose>> = (0..n).map(|_| None).collect();
    for (local, &f) in tree.used.iter().enumerate() {
        let (ppx, ppy) = principal_points[f];
        cameras[f] = Some(CameraPose {
            focal_px: ba.focals[local],
            rotation: rotations[local],
            ppx,
            ppy,
        });
    }

    Ok(Registration {
        cameras,
        used_indices: tree.used,
        reference_index: tree.reference,
        pairwise,
        ba_final_rms: ba.final_rms,
        ba_converged: ba.converged,
    })
}

/// Register `frames` and composite them into a finished panorama.
///
/// This is [`register`] followed by [`compose`]. `progress` is invoked at the start of each phase:
/// `Register` and `BundleAdjust` inside [`register`], then `Warp`, `Blend`, and `Crop` here.
pub fn stitch(
    frames: &[Frame],
    opt: &StitchOptions,
    progress: &(dyn Fn(Phase) + Sync),
) -> Result<StitchResult, PanoError> {
    let reg = register(frames, progress)?;
    compose_inner(Source::Resident(frames), &reg, opt, progress, &never_cancel)
}

/// [`stitch`] over a lazily-materialized [`FrameSource`], holding at most one full-res frame at a
/// time and honoring `cancel` between every phase and every frame.
///
/// Two passes over the source: a low-resolution pass (every frame at the ~1400 px seam resolution)
/// that drives registration, exposure gains and seam masks, then a full-resolution pass over the
/// *used* frames only — each loaded, warped into the blender, and dropped before the next is asked
/// for.
pub fn stitch_streaming(
    src: &dyn FrameSource,
    opt: &StitchOptions,
    progress: &(dyn Fn(Phase) + Sync),
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<StitchResult, PanoError> {
    progress(Phase::Register);
    let lows = load_low_pass(src, cancel)?;
    let reg = {
        let inputs: Vec<RegInput<'_>> = lows.iter().map(RegInput::loaded).collect();
        register_inner(&inputs, progress, cancel)?
    };
    compose_inner(Source::Streaming { src, lows }, &reg, opt, progress, cancel)
}

/// Materialize every frame at the ~1400 px seam resolution, sequentially — at most one full-res
/// decode is live inside `load` at a time, and only the small buffer survives the call.
fn load_low_pass(
    src: &dyn FrameSource,
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<LoadedFrame>, PanoError> {
    let n = src.len();
    let mut lows = Vec::with_capacity(n);
    for i in 0..n {
        if cancel() {
            return Err(PanoError::Cancelled);
        }
        lows.push(src.load(i, Some(project::SEAM_LONG_SIDE as u32))?);
    }
    Ok(lows)
}

/// Where the compositor gets pixels from.
enum Source<'a> {
    /// Frames already in memory — sampled in place, at full resolution, for both passes.
    Resident(&'a [Frame]),
    /// Frames materialized on demand; `lows` is the resident low-resolution pass.
    Streaming {
        src: &'a dyn FrameSource,
        lows: Vec<LoadedFrame>,
    },
}

/// A full-resolution frame held for exactly one blend step: borrowed for a resident source, owned
/// (and dropped at the end of the step) for a streaming one.
enum Held<'a> {
    Borrowed(&'a Frame),
    Owned(LoadedFrame),
}

impl Held<'_> {
    fn view(&self) -> project::FrameView<'_> {
        match self {
            Held::Borrowed(f) => project::FrameView {
                rgb: &f.rgb,
                width: f.width,
                height: f.height,
            },
            Held::Owned(f) => project::FrameView {
                rgb: &f.rgb,
                width: f.width,
                height: f.height,
            },
        }
    }
}

impl Source<'_> {
    /// FULL-RES `(width, height)` of every input frame — geometry only, no pixels touched.
    fn dims(&self) -> Vec<(usize, usize)> {
        match self {
            Source::Resident(frames) => frames.iter().map(|f| (f.width, f.height)).collect(),
            Source::Streaming { lows, .. } => {
                lows.iter().map(|f| (f.full_width, f.full_height)).collect()
            }
        }
    }

    /// The buffer the exposure/seam pass samples for frame `i`, and its scale relative to full-res
    /// (1.0 for a resident source — hence byte-identical results there).
    fn low_view(&self, i: usize) -> (project::FrameView<'_>, f64) {
        match self {
            Source::Resident(frames) => {
                let f = &frames[i];
                (
                    project::FrameView {
                        rgb: &f.rgb,
                        width: f.width,
                        height: f.height,
                    },
                    1.0,
                )
            }
            Source::Streaming { lows, .. } => {
                let f = &lows[i];
                let ratio = f.width.max(f.height) as f64 / f.full_width.max(f.full_height) as f64;
                (
                    project::FrameView {
                        rgb: &f.rgb,
                        width: f.width,
                        height: f.height,
                    },
                    ratio,
                )
            }
        }
    }

    /// Drop the low-resolution pass once seams and gains are solved (no-op when resident, whose
    /// buffers are the caller's).
    fn release_low(&mut self) {
        if let Source::Streaming { lows, .. } = self {
            lows.clear();
            lows.shrink_to_fit();
        }
    }

    /// Frame `i` at FULL resolution for the blend step.
    fn full(&self, i: usize) -> Result<Held<'_>, PanoError> {
        match self {
            Source::Resident(frames) => Ok(Held::Borrowed(&frames[i])),
            Source::Streaming { src, .. } => Ok(Held::Owned(src.load(i, None)?)),
        }
    }
}

/// Composite an already-computed [`Registration`] into a panorama (lets a caller reuse a registration).
///
/// Pipeline: pick the projection surface (honoring [`StitchOptions::projection`], with an auto rule and
/// a wide-Perspective→Cylindrical fallback) → lay out the canvas (border-sampled extent, capped to
/// [`StitchOptions::max_long_side`]) → low-res warp for exposure gains + seam masks → full-res
/// streaming multi-band blend → optional auto-crop. Frames outside `reg.used_indices` are ignored.
///
/// Emits `Phase::Warp`, `Phase::Blend`, `Phase::Crop` at the corresponding stage starts.
pub fn compose(
    frames: &[Frame],
    reg: &Registration,
    opt: &StitchOptions,
    progress: &(dyn Fn(Phase) + Sync),
) -> Result<StitchResult, PanoError> {
    compose_inner(Source::Resident(frames), reg, opt, progress, &never_cancel)
}

/// Shared body of [`compose`] / [`stitch_streaming`]'s compositing half.
///
/// For [`Source::Resident`] every `low_view` is the full-res buffer at ratio 1, which makes the
/// camera rescale below an identity and keeps this path bit-for-bit the pre-streaming compositor.
fn compose_inner(
    mut source: Source<'_>,
    reg: &Registration,
    opt: &StitchOptions,
    progress: &(dyn Fn(Phase) + Sync),
    cancel: &(dyn Fn() -> bool + Sync),
) -> Result<StitchResult, PanoError> {
    progress(Phase::Warp);

    let dims = source.dims();
    let cams = project::build_used_cams(&dims, reg);
    let ref_slot = reg
        .used_indices
        .iter()
        .position(|&f| f == reg.reference_index)
        .unwrap_or(0);

    let (h_span, v_span) = project::angular_spans(&cams);
    let projection = project::resolve_projection(opt.projection, h_span, v_span);
    let (map, capped) = project::build_map(&cams, ref_slot, projection, opt.max_long_side);

    // Low-resolution warps drive exposure compensation and seam finding; hold them all (small).
    // The CANVAS scale (`seam_ratio`) and the SOURCE scale (`low_view`'s ratio) are independent: the
    // canvas always drops to ~SEAM_LONG_SIDE, while the source is whatever the frame source handed
    // over — full-res when resident, ~SEAM_LONG_SIDE when streaming.
    let low_map = map.scaled(project::seam_ratio(&map));
    let mut low_warps: Vec<Option<project::Warped>> = Vec::with_capacity(cams.len());
    for (cam, &fidx) in cams.iter().zip(reg.used_indices.iter()) {
        if cancel() {
            return Err(PanoError::Cancelled);
        }
        let (view, ratio) = source.low_view(fidx);
        let low_cam = cam.scaled(ratio, view.width, view.height);
        low_warps.push(project::warp(&low_cam, view, &low_map));
    }
    if cancel() {
        return Err(PanoError::Cancelled);
    }

    let gains = exposure::solve_gains(&low_warps);
    let seams = seam::build_id_map(
        &low_warps,
        &reg.used_indices,
        &reg.pairwise,
        &gains,
        low_map.width,
        low_map.height,
    );
    drop(low_warps);
    // Nothing reads the low-resolution buffers past this point; free them before the blend loop
    // starts pulling full-res frames, so the two never coexist.
    source.release_low();

    progress(Phase::Blend);

    let mut blender = blend::Blender::new(map.width, map.height);
    for (slot, (&fidx, cam)) in reg.used_indices.iter().zip(cams.iter()).enumerate() {
        if cancel() {
            return Err(PanoError::Cancelled);
        }
        // `held` owns the full-res buffer on the streaming path and is dropped at the end of this
        // iteration — at most one full-res source is ever resident.
        let held = source.full(fidx)?;
        if let Some(w) = project::warp(cam, held.view(), &map) {
            blender.add(&w, slot, &seams, gains[slot]);
        }
    }
    if cancel() {
        return Err(PanoError::Cancelled);
    }
    let (mut rgb, mut valid) = blender.finish();
    let mut width = map.width;
    let mut height = map.height;

    // Boundary warp / rectangling (Phase P5): warp the composite so its valid region fills more of the
    // rectangle, so the auto-crop below keeps far more content. Gated on `boundary_warp > 0` (the
    // slider is 0..100 → lerp strength t = warp/100); at 0 the pass is skipped entirely, keeping the
    // pre-P5 output byte-identical.
    if opt.boundary_warp > 0.0 {
        progress(Phase::Rectangle);
        let t = (opt.boundary_warp as f64 / 100.0).clamp(0.0, 1.0);
        if let Some((wrgb, wvalid)) = rectangle::rectangle_warp(&rgb, &valid, width, height, t) {
            rgb = wrgb;
            valid = wvalid;
        }
    }

    progress(Phase::Crop);

    if opt.auto_crop {
        let (x0, y0, cw, ch) = crop::largest_inscribed_rect(&valid, width, height);
        // Skip if the inscribed rect degenerates (< 16 px either side) or would not actually crop.
        if cw >= 16 && ch >= 16 && (cw < width || ch < height) {
            let mut nrgb = vec![0.0f32; cw * ch * 3];
            let mut nvalid = vec![0u8; cw * ch];
            for y in 0..ch {
                for x in 0..cw {
                    let src = (y0 + y) * width + (x0 + x);
                    let dst = y * cw + x;
                    nrgb[dst * 3] = rgb[src * 3];
                    nrgb[dst * 3 + 1] = rgb[src * 3 + 1];
                    nrgb[dst * 3 + 2] = rgb[src * 3 + 2];
                    nvalid[dst] = valid[src];
                }
            }
            rgb = nrgb;
            valid = nvalid;
            width = cw;
            height = ch;
        }
    }

    Ok(StitchResult {
        width,
        height,
        rgb,
        valid_mask: valid,
        reference_index: reg.reference_index,
        used_indices: reg.used_indices.clone(),
        projection_used: projection,
        capped,
    })
}

/// Flatten verified inliers into bundle-adjustment observations, subsampling each pair to at most
/// [`BA_MAX_INLIERS_PER_PAIR`] with a deterministic stride.
fn build_observations(
    pairwise: &[PairMatch],
    used: &[usize],
    local_of: &[usize],
) -> Vec<bundle::Obs> {
    let used_set: std::collections::HashSet<usize> = used.iter().copied().collect();
    let mut obs = Vec::new();
    for pm in pairwise {
        if !used_set.contains(&pm.i) || !used_set.contains(&pm.j) {
            continue;
        }
        let (ci, cj) = (local_of[pm.i], local_of[pm.j]);
        let m = pm.inliers.len();
        let stride = m.div_ceil(BA_MAX_INLIERS_PER_PAIR).max(1);
        for (k, (p, q)) in pm.inliers.iter().enumerate() {
            if k % stride != 0 {
                continue;
            }
            obs.push(bundle::Obs {
                cam_i: ci,
                cam_j: cj,
                p: (p.x, p.y),
                q: (q.x, q.y),
            });
        }
    }
    obs
}
