//! Synthetic end-to-end compositing tests (Phase P2): warp → exposure → seam → multi-band blend →
//! crop, over the same deterministic rotating-camera scene used by the P1 registration test.

mod common;

use std::sync::Mutex;

use common::{make_base, three_frames, BASE_H, BASE_W, VIEW_H, VIEW_W};
use core_pano::{stitch, Phase, Projection, StitchOptions};

fn opts(projection: Projection, auto_crop: bool, max_long_side: u32) -> StitchOptions {
    StitchOptions {
        projection,
        boundary_warp: 0.0,
        auto_crop,
        max_long_side,
        preview: false,
    }
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

#[test]
fn stitch_three_frames_cylindrical_is_seamless() {
    let frames = three_frames([1.0, 1.0, 1.0]);

    // --- Phase emission is recorded on the cropped run. ---
    let phases = Mutex::new(Vec::<String>::new());
    let record = |p: Phase| phases.lock().unwrap().push(format!("{p:?}"));
    let cropped = stitch(&frames, &opts(Projection::Cylindrical, true, 4000), &record)
        .expect("stitch should succeed");

    let seen = phases.lock().unwrap().clone();
    for want in ["Register", "BundleAdjust", "Warp", "Blend", "Crop"] {
        assert!(
            seen.iter().any(|p| p == want),
            "missing phase {want}: {seen:?}"
        );
    }

    // Cropped output: dims sane, fully covered, un-capped, projection honored.
    assert_eq!(cropped.projection_used, Projection::Cylindrical);
    assert!(!cropped.capped, "4000 px cap should not trigger");
    assert!(
        cropped.width > VIEW_W,
        "pano width {} should exceed a single frame ({VIEW_W})",
        cropped.width
    );
    assert!(
        cropped.height as f64 >= 0.8 * VIEW_H as f64,
        "pano height {} should be at least 0.8x a single frame",
        cropped.height
    );
    assert!(
        cropped.valid_mask.iter().all(|&v| v == 1),
        "auto-cropped output must be fully covered"
    );
    assert_eq!(cropped.reference_index, 1, "central frame is the reference");

    // --- Seam continuity on the UNCROPPED result. ---
    let full = stitch(
        &frames,
        &opts(Projection::Cylindrical, false, 4000),
        &|_| {},
    )
    .expect("stitch should succeed");
    let (w, h) = (full.width, full.height);

    let base = make_base();
    let base_max = max_grad(&base, BASE_W, BASE_H);
    let threshold = 5.0 * base_max;

    // ~2000 deterministic interior samples whose full 3x3 neighbourhood is covered.
    let total = w * h;
    let stride = (total / 2000).max(1);
    let mut tested = 0usize;
    let mut worst = 0.0f32;
    for idx in (0..total).step_by(stride) {
        let (x, y) = (idx % w, idx / w);
        if x < 1 || y < 1 || x + 1 >= w || y + 1 >= h {
            continue;
        }
        let mut all_valid = true;
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let nx = (x as i32 + dx) as usize;
                let ny = (y as i32 + dy) as usize;
                if full.valid_mask[ny * w + nx] == 0 {
                    all_valid = false;
                }
            }
        }
        if !all_valid {
            continue;
        }
        let g = grad_mag(&full.rgb, w, h, x, y);
        worst = worst.max(g);
        assert!(
            g <= threshold,
            "hard seam at ({x},{y}): grad {g} > 5x base {base_max} = {threshold}"
        );
        tested += 1;
    }
    assert!(tested > 100, "too few interior samples tested: {tested}");
    println!(
        "seam continuity: {tested} samples, worst grad {worst:.4}, base max {base_max:.4}, \
         threshold {threshold:.4}; canvas {w}x{h}"
    );
}

/// Mean luma of each column-third over the covered pixels of a result.
fn thirds_means(res: &core_pano::StitchResult) -> [f64; 3] {
    let (w, h) = (res.width, res.height);
    let third = (w / 3).max(1);
    let mut means = [0.0f64; 3];
    let mut counts = [0.0f64; 3];
    for y in 0..h {
        for x in 0..w {
            let t = (x / third).min(2);
            if res.valid_mask[y * w + x] == 1 {
                means[t] += luma(&res.rgb, y * w + x) as f64;
                counts[t] += 1.0;
            }
        }
    }
    for k in 0..3 {
        assert!(counts[k] > 0.0, "third {k} had no covered pixels");
        means[k] /= counts[k];
    }
    means
}

#[test]
fn exposure_compensation_evens_out_thirds() {
    // Frame 0 darkened ×0.7, frame 2 brightened ×1.4 before stitching; gain compensation should undo
    // those per-frame offsets so the panorama looks evenly exposed.
    //
    // We measure this against the no-exposure baseline rather than as an absolute cross-thirds spread:
    // this synthetic scene is itself ~10% non-uniform across the thirds (each third views a different
    // slice of the base texture), so an absolute test would measure scene content, not exposure. If
    // compensation is working, the exposed panorama differs from the baseline by only a single global
    // gain in every third — i.e. the per-third exp/baseline ratios are equal (well within 8%).
    let baseline = stitch(
        &three_frames([1.0, 1.0, 1.0]),
        &opts(Projection::Cylindrical, true, 4000),
        &|_| {},
    )
    .expect("baseline stitch should succeed");
    let exposed = stitch(
        &three_frames([0.7, 1.0, 1.4]),
        &opts(Projection::Cylindrical, true, 4000),
        &|_| {},
    )
    .expect("exposed stitch should succeed");

    let base_m = thirds_means(&baseline);
    let exp_m = thirds_means(&exposed);
    let ratios = [
        exp_m[0] / base_m[0],
        exp_m[1] / base_m[1],
        exp_m[2] / base_m[2],
    ];
    let mean_ratio = (ratios[0] + ratios[1] + ratios[2]) / 3.0;
    let spread = ratios.iter().cloned().fold(f64::MIN, f64::max)
        - ratios.iter().cloned().fold(f64::MAX, f64::min);
    assert!(
        spread < 0.08 * mean_ratio,
        "per-third exp/baseline ratios {ratios:?} spread {spread} exceeds 8% of {mean_ratio}; \
         exposure was not evenly compensated"
    );
    println!(
        "exposure recovery: baseline thirds {base_m:?}, exposed {exp_m:?}, ratios {ratios:?} \
         (spread {spread:.5})"
    );
}

#[test]
fn size_cap_forces_downscale() {
    let frames = three_frames([1.0, 1.0, 1.0]);
    let res = stitch(&frames, &opts(Projection::Cylindrical, false, 800), &|_| {})
        .expect("stitch should succeed");
    let long = res.width.max(res.height);
    assert!(res.capped, "800 px cap should trigger on this scene");
    assert!(long <= 800, "capped long side {long} must be <= 800");
    assert!(long >= 700, "capped long side {long} unexpectedly small");
    println!(
        "size cap: {}x{} (long {long}), capped={}",
        res.width, res.height, res.capped
    );
}

#[test]
fn stitches_two_frames_without_panic() {
    // Minimum viable panorama: two overlapping frames must complete every phase without panicking.
    let base = make_base();
    let frames = vec![
        common::render_view(&base, -8.0, 1.0),
        common::render_view(&base, 8.0, 1.0),
    ];
    let res = stitch(&frames, &opts(Projection::Auto, true, 4000), &|_| {})
        .expect("two-frame stitch should succeed");
    assert!(res.width > 0 && res.height > 0);
    assert_ne!(res.projection_used, Projection::Auto);
}
