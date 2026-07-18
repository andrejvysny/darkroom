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

mod blend;
mod bundle;
mod camera;
mod crop;
mod exposure;
mod features;
mod graph;
mod matching;
mod project;
mod ransac;
mod rng;
mod seam;
mod wave;

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
    progress(Phase::Register);

    let n = frames.len();
    if n < 2 {
        return Err(PanoError::TooFewMatched(n));
    }

    // --- Features (parallel over frames) ---
    let pattern = features::fixed_test_pairs();
    let feats: Vec<features::FrameFeatures> = frames
        .par_iter()
        .map(|f| features::extract(f, &pattern))
        .collect();

    // --- Matching + RANSAC + verification (parallel over unordered pairs) ---
    let pair_list: Vec<(usize, usize)> = (0..n)
        .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
        .collect();

    let pairwise: Vec<PairMatch> = pair_list
        .par_iter()
        .filter_map(|&(i, j)| {
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
    let principal_points: Vec<(f64, f64)> = frames
        .iter()
        .map(|f| (f.width as f64 * 0.5, f.height as f64 * 0.5))
        .collect();

    // Focal from every verified homography; fall back to the reference frame's seed / a wide guess.
    let homs: Vec<Matrix3<f64>> = pairwise.iter().map(|pm| pm.h).collect();
    let focal = camera::estimate_focal(
        &homs,
        frames[tree.reference].width as f64,
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
    compose(frames, &reg, opt, progress)
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
    // `boundary_warp` is reserved for v-next boundary warp / rectangling (Phase::Rectangle); it is
    // accepted but has no effect in v1.
    let _ = opt.boundary_warp;

    progress(Phase::Warp);

    let cams = project::build_used_cams(frames, reg);
    let ref_slot = reg
        .used_indices
        .iter()
        .position(|&f| f == reg.reference_index)
        .unwrap_or(0);

    let (h_span, v_span) = project::angular_spans(&cams);
    let projection = project::resolve_projection(opt.projection, h_span, v_span);
    let (map, capped) = project::build_map(&cams, ref_slot, projection, opt.max_long_side);

    // Low-resolution warps drive exposure compensation and seam finding; hold them all (small).
    let (low_map, low_warps) = project::warp_lowres(&cams, &reg.used_indices, frames, &map);
    let gains = exposure::solve_gains(&low_warps);
    let seams = seam::build_id_map(
        &low_warps,
        &reg.used_indices,
        &reg.pairwise,
        low_map.width,
        low_map.height,
    );
    drop(low_warps);

    progress(Phase::Blend);

    let mut blender = blend::Blender::new(map.width, map.height);
    for (slot, (&fidx, cam)) in reg.used_indices.iter().zip(cams.iter()).enumerate() {
        if let Some(w) = project::warp(cam, &frames[fidx], &map) {
            blender.add(&w, slot, &seams, gains[slot]);
        }
    }
    let (mut rgb, mut valid) = blender.finish();
    let mut width = map.width;
    let mut height = map.height;

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
