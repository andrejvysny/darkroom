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

fn opts_warp(
    projection: Projection,
    auto_crop: bool,
    max_long_side: u32,
    boundary_warp: f32,
) -> StitchOptions {
    StitchOptions {
        projection,
        boundary_warp,
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

/// Pearson correlation of Rec.601 luma between two equal-sized composites over their central 40% box.
fn central_luma_corr(a: &[f32], b: &[f32], w: usize, h: usize) -> f64 {
    let (x0, x1) = (w * 3 / 10, w * 7 / 10);
    let (y0, y1) = (h * 3 / 10, h * 7 / 10);
    let lum = |p: &[f32], i: usize| {
        0.299 * p[i * 3] as f64 + 0.587 * p[i * 3 + 1] as f64 + 0.114 * p[i * 3 + 2] as f64
    };
    let (mut sa, mut sb, mut n) = (0.0, 0.0, 0.0);
    for y in y0..y1 {
        for x in x0..x1 {
            sa += lum(a, y * w + x);
            sb += lum(b, y * w + x);
            n += 1.0;
        }
    }
    let (ma, mb) = (sa / n, sb / n);
    let (mut cab, mut caa, mut cbb) = (0.0, 0.0, 0.0);
    for y in y0..y1 {
        for x in x0..x1 {
            let da = lum(a, y * w + x) - ma;
            let db = lum(b, y * w + x) - mb;
            cab += da * db;
            caa += da * da;
            cbb += db * db;
        }
    }
    cab / (caa.sqrt() * cbb.sqrt())
}

#[test]
fn boundary_warp_enlarges_crop_and_preserves_interior() {
    // (Priority-1 test 1) With boundary_warp=100 the valid region is warped toward the rectangle, so
    // the auto-crop keeps a materially larger area (>= 1.15x), while the interior content is preserved
    // (central-region luma correlation between the warped and unwarped composite >= 0.9).
    let frames = three_frames([1.0, 1.0, 1.0]);

    let cropped0 = stitch(&frames, &opts(Projection::Cylindrical, true, 4000), &|_| {})
        .expect("warp=0 stitch");
    let cropped1 = stitch(
        &frames,
        &opts_warp(Projection::Cylindrical, true, 4000, 100.0),
        &|_| {},
    )
    .expect("warp=100 stitch");

    let a0 = cropped0.width * cropped0.height;
    let a1 = cropped1.width * cropped1.height;
    let ratio = a1 as f64 / a0 as f64;
    assert!(
        ratio >= 1.15,
        "boundary warp should grow the cropped area >= 1.15x: {}x{} -> {}x{} (ratio {ratio:.3})",
        cropped0.width,
        cropped0.height,
        cropped1.width,
        cropped1.height
    );

    // Interior preservation is measured on the UNCROPPED composites (identical dimensions).
    let full0 = stitch(
        &frames,
        &opts(Projection::Cylindrical, false, 4000),
        &|_| {},
    )
    .expect("warp=0 uncropped");
    let full1 = stitch(
        &frames,
        &opts_warp(Projection::Cylindrical, false, 4000, 100.0),
        &|_| {},
    )
    .expect("warp=100 uncropped");
    assert_eq!(
        (full0.width, full0.height),
        (full1.width, full1.height),
        "warp keeps the canvas dimensions"
    );
    let corr = central_luma_corr(&full1.rgb, &full0.rgb, full0.width, full0.height);
    assert!(
        corr >= 0.9,
        "boundary warp must preserve interior content, central correlation {corr:.4} < 0.9"
    );
    println!("boundary warp: crop area ratio {ratio:.3}, central correlation {corr:.4}");
}

#[test]
fn boundary_warp_zero_is_identical_and_phaseless() {
    // (Priority-1 test 2) warp=0 leaves the pre-P5 path untouched: no Rectangle phase is emitted and
    // the output is byte-identical run to run, and it differs from a real (warp>0) rectangling run.
    let frames = three_frames([1.0, 1.0, 1.0]);

    let phases = Mutex::new(Vec::<String>::new());
    let rec = |p: Phase| phases.lock().unwrap().push(format!("{p:?}"));
    let a = stitch(&frames, &opts(Projection::Cylindrical, false, 4000), &rec).expect("warp=0 a");
    assert!(
        !phases.lock().unwrap().iter().any(|p| p == "Rectangle"),
        "warp=0 must not emit the Rectangle phase: {:?}",
        phases.lock().unwrap()
    );

    let b = stitch(
        &frames,
        &opts(Projection::Cylindrical, false, 4000),
        &|_| {},
    )
    .expect("warp=0 b");
    assert_eq!(
        a.rgb, b.rgb,
        "warp=0 rgb must be byte-identical (deterministic no-op)"
    );
    assert_eq!(
        a.valid_mask, b.valid_mask,
        "warp=0 mask must be byte-identical"
    );

    // A real rectangling run emits Rectangle and changes the pixels.
    let wphases = Mutex::new(Vec::<String>::new());
    let wrec = |p: Phase| wphases.lock().unwrap().push(format!("{p:?}"));
    let c = stitch(
        &frames,
        &opts_warp(Projection::Cylindrical, false, 4000, 100.0),
        &wrec,
    )
    .expect("warp=100");
    assert!(
        wphases.lock().unwrap().iter().any(|p| p == "Rectangle"),
        "warp>0 must emit the Rectangle phase"
    );
    assert!(c.rgb != a.rgb, "warp>0 must actually change the composite");
}

#[test]
fn deghosting_keeps_moving_object_single_source() {
    // (Priority-3 test) A bright square painted into ONLY frame 0's overlap region simulates a moving
    // object. A ghosting seam that ran through it would blend the square against frame 1's (dark)
    // background, gashing the square with dark pixels. Assert instead that the square survives intact
    // and single-source: present at full brightness, ~its injected size, with a near-uniform interior
    // (no dark blend gash → low interior gradient).
    let base = make_base();
    let sq = 60usize;
    let mut f0 = common::render_view(&base, -12.0, 1.0);
    // Paint the square into frame 0's right portion, which overlaps frame 1.
    for y in 220..(220 + sq) {
        for x in 560..(560 + sq) {
            let i = (y * VIEW_W + x) * 3;
            f0.rgb[i] = 1.0;
            f0.rgb[i + 1] = 1.0;
            f0.rgb[i + 2] = 1.0;
        }
    }
    let frames = vec![
        f0,
        common::render_view(&base, 0.0, 1.0),
        common::render_view(&base, 12.0, 1.0),
    ];
    let res = stitch(
        &frames,
        &opts(Projection::Cylindrical, false, 4000),
        &|_| {},
    )
    .expect("stitch should succeed");
    let (w, h) = (res.width, res.height);

    // Locate the surviving bright square (luma > 0.7 over covered pixels).
    let (mut maxl, mut nbright) = (0.0f32, 0usize);
    let (mut bx0, mut by0, mut bx1, mut by1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if res.valid_mask[y * w + x] == 0 {
                continue;
            }
            let l = luma(&res.rgb, y * w + x);
            maxl = maxl.max(l);
            if l > 0.7 {
                nbright += 1;
                bx0 = bx0.min(x);
                by0 = by0.min(y);
                bx1 = bx1.max(x);
                by1 = by1.max(y);
            }
        }
    }
    assert!(
        maxl >= 0.9,
        "moving object lost brightness (ghosted): max luma {maxl:.3}"
    );
    assert!(
        nbright >= (sq * sq) / 2,
        "too little of the object survived: {nbright} bright px (injected {sq}x{sq})"
    );

    // Interior of the square must be a solid single source: shrink the bright bbox by a margin and
    // check the max luma gradient there is small (a seam gash would show a dark step ~1.0).
    let m = 10usize;
    assert!(
        bx1 > bx0 + 2 * m && by1 > by0 + 2 * m,
        "bright region too small to probe"
    );
    let mut inner_max_grad = 0.0f32;
    for y in (by0 + m)..(by1 - m) {
        for x in (bx0 + m)..(bx1 - m) {
            inner_max_grad = inner_max_grad.max(grad_mag(&res.rgb, w, h, x, y));
        }
    }
    assert!(
        inner_max_grad < 0.3,
        "seam gash through the object: interior max gradient {inner_max_grad:.3}"
    );
    println!(
        "deghosting: object survived {}x{} (max luma {maxl:.3}, {nbright} bright px, \
         interior max grad {inner_max_grad:.4})",
        bx1 - bx0 + 1,
        by1 - by0 + 1
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
