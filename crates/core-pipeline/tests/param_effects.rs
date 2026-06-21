//! Golden checks that each develop param maps to the correct GPU uniform slot and has the
//! expected effect. This is also the regression guard for the std140 uniform layout
//! (`vec3 wb_gain` + scalars) — a misalignment would make exposure/saturation/etc. no-ops.
//! Skips gracefully when no GPU adapter is available.

use core_pipeline::{DevelopParams, DevelopPipeline, GpuContext};
use core_raw::LinearImage;

fn solid(w: u32, h: u32, rgb: [f32; 3]) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..(w * h) {
        data.extend_from_slice(&rgb);
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

fn mean_channel(rgba: &[u8], ch: usize) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        sum += px[ch] as u64;
        n += 1;
    }
    sum as f64 / n as f64
}

#[test]
fn develop_params_have_correct_effects() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    let gray = solid(16, 16, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();

    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let l_base = mean_channel(&base, 0);

    let up = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                exposure: 1.5,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        mean_channel(&up, 0) > l_base + 10.0,
        "exposure+ must brighten (base {l_base}, got {})",
        mean_channel(&up, 0)
    );

    let down = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                exposure: -1.5,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        mean_channel(&down, 0) < l_base - 10.0,
        "exposure- must darken (base {l_base}, got {})",
        mean_channel(&down, 0)
    );

    let warm = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                temp: 80.0,
                ..Default::default()
            },
        )
        .unwrap();
    let (r, b) = (mean_channel(&warm, 0), mean_channel(&warm, 2));
    assert!(r > b + 8.0, "temp+ must make R>B (r {r}, b {b})");

    // Saturation widens the R-B spread on a colored patch.
    let red = solid(16, 16, [0.45, 0.12, 0.12]);
    let prep2 = pipe.prepare(&ctx, &red).unwrap();
    let s0 = pipe
        .render(&ctx, &prep2, &DevelopParams::default())
        .unwrap();
    let s1 = pipe
        .render(
            &ctx,
            &prep2,
            &DevelopParams {
                saturation: 80.0,
                ..Default::default()
            },
        )
        .unwrap();
    let spread0 = mean_channel(&s0, 0) - mean_channel(&s0, 2);
    let spread1 = mean_channel(&s1, 0) - mean_channel(&s1, 2);
    assert!(
        spread1 > spread0,
        "saturation+ must increase R-B spread ({spread0} -> {spread1})"
    );
}

/// Scene-referred base tone operator (ACR-matched, full Amount default): maps scene-linear ProPhoto
/// gray through the display transition to the expected ACR-default 8-bit tonality. Locks the curve
/// shape (mid-grey anchor + smooth highlight compression + toe), replacing the old fixed `exp()`
/// shoulder. Vectors from the Codex curve review of the analytic `p=1.35` seed.
#[test]
fn base_curve_tone_response() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    let render_val = |v: f32| {
        let img = solid(8, 8, [v, v, v]);
        let prep = pipe.prepare(&ctx, &img).unwrap();
        let out = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
        // Solid input ⇒ every pixel identical ⇒ mean == the exact 8-bit output value.
        mean_channel(&out, 0)
    };

    // (scene-linear input, inclusive 8-bit output bounds). Mid-grey 0.18 must anchor near 118.
    let cases = [
        (0.0_f32, 0.0_f64, 1.0_f64),
        (0.01, 11.0, 17.0),
        (0.05, 51.0, 58.0),
        (0.18, 116.0, 120.0),
        (0.5, 178.0, 185.0),
        (1.0, 213.0, 220.0),
        (2.0, 234.0, 241.0),
        (4.0, 245.0, 251.0),
    ];
    let mut prev = -1.0;
    for (x, lo, hi) in cases {
        let got = render_val(x);
        assert!(
            got >= lo && got <= hi,
            "base curve f({x}) → 8-bit {got}, expected [{lo},{hi}]"
        );
        assert!(
            got > prev,
            "tone response must be monotonic ({prev} -> {got} at x={x})"
        );
        prev = got;
    }
}

/// The per-channel base curve desaturates highlights, but the hue-protection blend must keep a
/// saturated highlight the SAME hue (no neon twist, no flip to white). A bright saturated red stays
/// distinctly red (R ≫ B) in the output.
#[test]
fn base_curve_preserves_saturated_hue() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    // Bright, saturated red with headroom (>1.0) — the regime where per-channel curves twist hue.
    let red = solid(16, 16, [3.0, 0.3, 0.3]);
    let prep = pipe.prepare(&ctx, &red).unwrap();
    let out = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let (r, b) = (mean_channel(&out, 0), mean_channel(&out, 2));
    assert!(
        r > b + 30.0,
        "saturated red must stay red after tone mapping (r {r}, b {b})"
    );
}

/// Crop selects the right region (UV-remap). The `edge` image is dark-left | bright-right; cropping
/// to the right half makes the output center sample the bright side, the left half the dark side.
#[test]
fn crop_selects_region() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    let img = edge(64, 16);
    let prep = pipe.prepare(&ctx, &img).unwrap();

    let center = |crop: core_pipeline::Crop| {
        let out = pipe
            .render(
                &ctx,
                &prep,
                &DevelopParams {
                    crop,
                    ..Default::default()
                },
            )
            .unwrap();
        out[(8 * 64 + 32) * 4] as i32 // center pixel, R channel
    };

    let right = center(core_pipeline::Crop {
        cx: 0.75,
        cy: 0.5,
        hw: 0.25,
        hh: 0.5,
        angle: 0.0,
    });
    let left = center(core_pipeline::Crop {
        cx: 0.25,
        cy: 0.5,
        hw: 0.25,
        hh: 0.5,
        angle: 0.0,
    });
    assert!(
        right > left + 20,
        "crop must select the sampled region (right-center {right} vs left-center {left})"
    );
}

/// Straighten (rotation + autozoom) on a solid image must keep the center a clean, artifact-free
/// copy of the un-rotated render (rotating a uniform field is a no-op; autozoom must not bleed the
/// letterbox edge into the center).
#[test]
fn straighten_solid_has_no_center_artifacts() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    let img = solid(48, 32, [0.35, 0.35, 0.35]);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let flat = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let tilted = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                crop: core_pipeline::Crop {
                    angle: 8.0,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let at = |buf: &[u8]| buf[(16 * 48 + 24) * 4] as i32; // center pixel
    assert!(
        (at(&tilted) - at(&flat)).abs() <= 2,
        "straighten of a solid must leave the center unchanged ({} vs {})",
        at(&flat),
        at(&tilted)
    );
}

/// Two vertical halves (dark | bright) — a hard edge down the middle, for sharpen tests.
fn edge(w: u32, h: u32) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..h {
        for x in 0..w {
            let v = if x < w / 2 { 0.2 } else { 0.5 };
            data.extend_from_slice(&[v, v, v]);
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

#[test]
fn vignette_darkens_corners() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    let img = solid(32, 32, [0.4, 0.4, 0.4]);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let out = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                vignette: -100.0,
                ..Default::default()
            },
        )
        .unwrap();
    let at = |c: usize, r: usize| out[(r * 32 + c) * 4] as i32;
    let corner = at(0, 0);
    let center = at(16, 16);
    assert!(
        corner < center - 15,
        "negative vignette must darken the corner vs center ({corner} vs {center})"
    );
}

#[test]
fn sharpen_overshoots_edges_keeps_flats() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    // Flat region: sharpen is a no-op (base − blur ≈ 0).
    let flat = solid(32, 32, [0.4, 0.4, 0.4]);
    let pf = pipe.prepare(&ctx, &flat).unwrap();
    let base_flat = pipe.render(&ctx, &pf, &DevelopParams::default()).unwrap();
    let sharp_flat = pipe
        .render(
            &ctx,
            &pf,
            &DevelopParams {
                sharpen: 150.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(base_flat, sharp_flat, "sharpen must not touch flat regions");

    // Hard edge: sharpen overshoots — the bright pixel adjacent to the edge gets brighter.
    let img = edge(64, 16);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let sharp = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                sharpen: 150.0,
                ..Default::default()
            },
        )
        .unwrap();
    // Column 32 is the first bright column (right of the edge at x=32).
    let at = |buf: &[u8], col: usize| buf[(8 * 64 + col) * 4] as i32;
    assert!(
        at(&sharp, 32) > at(&base, 32),
        "sharpen must overshoot the bright edge ({} -> {})",
        at(&base, 32),
        at(&sharp, 32)
    );
}
