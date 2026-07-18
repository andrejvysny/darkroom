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

mod bundle;
mod camera;
mod features;
mod graph;
mod matching;
mod ransac;
mod rng;
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
