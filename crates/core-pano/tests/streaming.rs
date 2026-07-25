//! Streaming-source tests (Track C): `FrameSource` load discipline, cancellation, and agreement
//! between the streaming stitcher and the resident-slice one.
//!
//! The streaming path deliberately does NOT reproduce `stitch(&frames)` bit-for-bit once the frames
//! are larger than the ~1400 px seam resolution: registration and the exposure/seam pass then sample
//! an area-downscaled copy rather than the full-res original. What must hold is that the *geometry*
//! (poses, canvas) is the same to within a small epsilon and that the composite is still seamless —
//! which is what these tests assert, alongside the load-count contract that makes the memory saving
//! real.

mod common;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use common::{angle_of, make_base, render_view_scaled, three_frames, BASE_H, BASE_W};
use core_pano::{
    register, register_streaming, stitch, stitch_streaming, Frame, FrameSource, LoadedFrame,
    PanoError, Phase, Projection, Registration, StitchOptions,
};

/// Views this much bigger than the canonical 700×500, so their long side (1540 px) exceeds the
/// stitcher's ~1400 px seam resolution and the streaming path genuinely downscales.
const BIG_SCALE: f64 = 2.2;

fn opts(max_long_side: u32) -> StitchOptions {
    StitchOptions {
        projection: Projection::Cylindrical,
        boundary_warp: 0.0,
        auto_crop: false,
        max_long_side,
        preview: false,
    }
}

/// A [`FrameSource`] over resident frames that counts how many times each index is materialized.
///
/// `load` delegates to the crate's own `&[Frame]` implementation, so the downscaling under test is
/// the real one; the counter is the only thing this adds.
struct VecFrameSource {
    frames: Vec<Frame>,
    loads: Mutex<Vec<usize>>,
}

impl VecFrameSource {
    fn new(frames: Vec<Frame>) -> VecFrameSource {
        let loads = Mutex::new(vec![0usize; frames.len()]);
        VecFrameSource { frames, loads }
    }

    fn counts(&self) -> Vec<usize> {
        self.loads.lock().unwrap().clone()
    }

    /// Long side actually handed over for index `i` at `max_long_side` — used to prove the low pass
    /// really is downscaled.
    fn delivered_long_side(&self, i: usize, max_long_side: Option<u32>) -> usize {
        let f = self.load(i, max_long_side).expect("load");
        f.width.max(f.height)
    }
}

impl FrameSource for VecFrameSource {
    fn len(&self) -> usize {
        self.frames.len()
    }

    fn load(&self, i: usize, max_long_side: Option<u32>) -> Result<LoadedFrame, PanoError> {
        if let Some(slot) = self.loads.lock().unwrap().get_mut(i) {
            *slot += 1;
        }
        let slice: &[Frame] = &self.frames;
        FrameSource::load(&slice, i, max_long_side)
    }
}

fn no_cancel() -> bool {
    false
}

/// Rec.601 luma of an interleaved-RGB pixel.
#[inline]
fn luma(rgb: &[f32], i: usize) -> f32 {
    0.299 * rgb[i * 3] + 0.587 * rgb[i * 3 + 1] + 0.114 * rgb[i * 3 + 2]
}

/// Central-difference luma gradient magnitude at an interior pixel.
fn grad_mag(rgb: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    debug_assert!(x >= 1 && y >= 1 && x + 1 < w && y + 1 < h);
    let gx = luma(rgb, y * w + x + 1) - luma(rgb, y * w + x - 1);
    let gy = luma(rgb, (y + 1) * w + x) - luma(rgb, (y - 1) * w + x);
    (gx * gx + gy * gy).sqrt()
}

/// Max luma gradient magnitude over the interior of a plane.
fn max_grad(rgb: &[f32], w: usize, h: usize) -> f32 {
    let mut m = 0.0f32;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            m = m.max(grad_mag(rgb, w, h, x, y));
        }
    }
    m
}

/// The three canonical pans, rendered at [`BIG_SCALE`].
fn big_frames() -> Vec<Frame> {
    let base = make_base();
    vec![
        render_view_scaled(&base, -12.0, 1.0, BIG_SCALE),
        render_view_scaled(&base, 0.0, 1.0, BIG_SCALE),
        render_view_scaled(&base, 12.0, 1.0, BIG_SCALE),
    ]
}

/// Pairwise relative-rotation angles (degrees) and per-frame focals, in `used_indices` order.
fn pose_summary(reg: &Registration) -> (Vec<f64>, Vec<f64>) {
    let cams: Vec<&core_pano::CameraPose> = reg
        .used_indices
        .iter()
        .map(|&i| reg.cameras[i].as_ref().expect("used frame has a pose"))
        .collect();
    let mut angles = Vec::new();
    for i in 0..cams.len() {
        for j in (i + 1)..cams.len() {
            angles.push(angle_of(&(cams[j].rotation * cams[i].rotation.transpose())).to_degrees());
        }
    }
    (angles, cams.iter().map(|c| c.focal_px).collect())
}

#[test]
fn every_frame_is_loaded_exactly_twice() {
    // The whole point of the streaming source: one small load per frame for registration + seams,
    // one full-res load per USED frame for the blend. Never a third, never two at once.
    let src = VecFrameSource::new(big_frames());
    let res = stitch_streaming(&src, &opts(2000), &|_| {}, &no_cancel).expect("streaming stitch");

    assert_eq!(
        res.used_indices.len(),
        3,
        "all three frames should register"
    );
    let counts = src.counts();
    // `delivered_long_side` probes below add loads of their own, so snapshot first.
    assert_eq!(
        counts,
        vec![2, 2, 2],
        "each used frame must be materialized exactly twice (low pass + blend), got {counts:?}"
    );

    // ...and the low pass must actually be the small one.
    let full_long = src.frames[0].width.max(src.frames[0].height);
    let low_long = src.delivered_long_side(0, Some(1400));
    assert!(
        full_long > 1400 && low_long <= 1400,
        "low pass should downscale {full_long} px to <= 1400, got {low_long}"
    );
    assert_eq!(
        src.delivered_long_side(0, None),
        full_long,
        "an uncapped load must deliver the full-res buffer"
    );
    println!("load counts {counts:?}; full long {full_long}, low long {low_long}");
}

#[test]
fn cancel_during_low_pass_returns_cancelled() {
    // Flip cancel once the source has been touched twice — i.e. part-way through the low pass, well
    // before registration starts.
    let src = VecFrameSource::new(three_frames([1.0, 1.0, 1.0]));
    let seen = AtomicUsize::new(0);
    let cancel = || {
        seen.fetch_add(1, Ordering::SeqCst);
        src.counts().iter().sum::<usize>() >= 2
    };
    let err = match stitch_streaming(&src, &opts(2000), &|_| {}, &cancel) {
        Ok(_) => panic!("a cancelled stitch must not return a result"),
        Err(e) => e,
    };
    assert!(
        matches!(err, PanoError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
    assert!(seen.load(Ordering::SeqCst) > 0, "cancel was never polled");
    let counts = src.counts();
    assert!(
        counts.iter().sum::<usize>() < 6,
        "cancellation should have stopped the loads early, got {counts:?}"
    );
}

#[test]
fn cancel_during_blend_returns_cancelled() {
    // Cancel only once compositing has begun, so the poll that catches it is the one inside the
    // full-res blend loop (registration and the seam pass are allowed to complete).
    let src = VecFrameSource::new(three_frames([1.0, 1.0, 1.0]));
    let blending = AtomicBool::new(false);
    let progress = |p: Phase| {
        if matches!(p, Phase::Blend) {
            blending.store(true, Ordering::SeqCst);
        }
    };
    let cancel = || blending.load(Ordering::SeqCst);
    let err = match stitch_streaming(&src, &opts(2000), &progress, &cancel) {
        Ok(_) => panic!("a cancelled stitch must not return a result"),
        Err(e) => e,
    };
    assert!(
        matches!(err, PanoError::Cancelled),
        "expected Cancelled, got {err:?}"
    );
    assert!(
        blending.load(Ordering::SeqCst),
        "the blend phase should have been reached"
    );
    // The low pass ran (3 loads); the blend was cut off before it could pull every full-res frame.
    let counts = src.counts();
    assert!(
        counts.iter().sum::<usize>() < 6,
        "blend loop should have bailed before loading every frame again, got {counts:?}"
    );
}

#[test]
fn streaming_registration_matches_resident_registration() {
    // The streaming registration detects in 1400 px buffers but reports poses in full-res units.
    // Different pixels in, so not bit-identical — but the recovered geometry must agree.
    let frames = big_frames();
    let resident = register(&frames, &|_| {}).expect("resident registration");
    let src = VecFrameSource::new(big_frames());
    let streamed = register_streaming(&src, &|_| {}, &no_cancel).expect("streaming registration");

    assert_eq!(streamed.used_indices, resident.used_indices);
    assert_eq!(streamed.reference_index, resident.reference_index);

    let (a_ang, a_foc) = pose_summary(&resident);
    let (b_ang, b_foc) = pose_summary(&streamed);
    for (a, b) in a_ang.iter().zip(b_ang.iter()) {
        assert!(
            (a - b).abs() < 0.3,
            "relative rotation drift {a:.4}° vs {b:.4}° exceeds 0.3°"
        );
    }
    for (a, b) in a_foc.iter().zip(b_foc.iter()) {
        assert!(
            (a - b).abs() < 0.02 * a,
            "focal drift {a:.1} vs {b:.1} exceeds 2%"
        );
    }
    println!("angles resident {a_ang:?} vs streamed {b_ang:?}; focals {a_foc:?} vs {b_foc:?}");
}

#[test]
fn streaming_stitch_matches_resident_stitch() {
    // Same canvas, same geometry, still seamless — measured with the compositing test's own
    // 5x-base-gradient seam threshold.
    let frames = big_frames();
    let resident = stitch(&frames, &opts(2000), &|_| {}).expect("resident stitch");
    let src = VecFrameSource::new(big_frames());
    let streamed = stitch_streaming(&src, &opts(2000), &|_| {}, &no_cancel).expect("streaming");

    assert_eq!(
        (streamed.width, streamed.height),
        (resident.width, resident.height),
        "streaming must lay out the same canvas"
    );
    assert_eq!(streamed.used_indices, resident.used_indices);
    assert_eq!(streamed.projection_used, resident.projection_used);
    assert_eq!(streamed.reference_index, resident.reference_index);

    // Coverage within a fraction of a percent (a pose epsilon moves the valid boundary slightly).
    let cov = |m: &[u8]| m.iter().filter(|&&v| v == 1).count() as f64 / m.len() as f64;
    let (ca, cb) = (cov(&resident.valid_mask), cov(&streamed.valid_mask));
    assert!(
        (ca - cb).abs() < 0.01,
        "valid coverage {cb:.5} should track the resident path's {ca:.5}"
    );

    let (w, h) = (streamed.width, streamed.height);
    let base_max = max_grad(&make_base(), BASE_W, BASE_H);
    let threshold = 5.0 * base_max;
    let stride = ((w * h) / 2000).max(1);
    let (mut tested, mut worst) = (0usize, 0.0f32);
    for idx in (0..w * h).step_by(stride) {
        let (x, y) = (idx % w, idx / w);
        if x < 1 || y < 1 || x + 1 >= w || y + 1 >= h {
            continue;
        }
        let covered = (-1i32..=1).all(|dy| {
            (-1i32..=1).all(|dx| {
                streamed.valid_mask[((y as i32 + dy) as usize) * w + (x as i32 + dx) as usize] == 1
            })
        });
        if !covered {
            continue;
        }
        let g = grad_mag(&streamed.rgb, w, h, x, y);
        worst = worst.max(g);
        assert!(
            g <= threshold,
            "hard seam in the streamed composite at ({x},{y}): grad {g} > {threshold}"
        );
        tested += 1;
    }
    assert!(tested > 100, "too few interior samples tested: {tested}");
    println!(
        "streaming vs resident: canvas {w}x{h}, coverage {cb:.5} vs {ca:.5}, \
         worst seam grad {worst:.4} (threshold {threshold:.4}, {tested} samples)"
    );
}

#[test]
fn slice_source_reproduces_the_resident_stitch_exactly() {
    // Frames small enough that the low pass is a no-op downscale collapse the streaming path onto
    // the resident one — a guard that the shared compositor did not drift.
    let frames = three_frames([1.0, 1.0, 1.0]);
    let resident = stitch(&frames, &opts(4000), &|_| {}).expect("resident stitch");
    let slice: &[Frame] = &frames;
    let streamed =
        stitch_streaming(&slice, &opts(4000), &|_| {}, &no_cancel).expect("slice-source stitch");

    assert_eq!(
        (streamed.width, streamed.height),
        (resident.width, resident.height)
    );
    assert_eq!(
        streamed.rgb, resident.rgb,
        "a sub-seam-resolution slice source must stitch byte-identically"
    );
    assert_eq!(streamed.valid_mask, resident.valid_mask);
}
