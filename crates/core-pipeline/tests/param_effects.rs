//! Golden checks that each develop param maps to the correct GPU uniform slot and has the
//! expected effect. This is also the regression guard for the std140 uniform layout
//! (`vec3 wb_gain` + scalars) — a misalignment would make exposure/saturation/etc. no-ops.
//! Skips gracefully when no GPU adapter is available.

use core_pipeline::{
    AiCoverage, AiPoint, CbRgb, ChannelMix, ComponentKind, DevelopParams, DevelopPipeline,
    GpuContext, LocalAdjust, Mask, MaskComponent, MaskOp,
};
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

/// Mean of channel `ch` over the column band `[x0, x1)` of a `w`-wide RGBA8 buffer.
fn mean_band(rgba: &[u8], w: u32, x0: u32, x1: u32, ch: usize) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    let stride = (w * 4) as usize;
    for row in rgba.chunks_exact(stride) {
        for x in x0..x1 {
            sum += row[(x * 4) as usize + ch] as u64;
            n += 1;
        }
    }
    sum as f64 / n.max(1) as f64
}

/// AI-mask (kind==5) coverage: a baked alpha stashed on the prepared image must brighten only the
/// region it covers. Feeds a left-half=255 / right-half=0 alpha (like a SAM result) into the mask
/// pre-pass via `PreparedImage::ai_coverages`, keyed by the `ComponentKind::Ai.hash`, with a +2 EV
/// local exposure. Guards the whole kind-5 path: ai_tex upload + `ai_scale` valid-region sampling +
/// per-mask local adjust. Deliberately `feather=false` so the boundary stays a hard half/half split.
#[test]
fn ai_mask_coverage_applies_locally() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    let gray = solid(64, 64, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();

    // Left half of the coverage selected, right half not. Sampled by normalized UV, so it splits the
    // rendered image down the middle regardless of the alpha's own resolution.
    let (aw, ah) = (64u32, 48u32);
    let mut alpha = vec![0u8; (aw * ah) as usize];
    for y in 0..ah {
        for x in 0..aw / 2 {
            alpha[(y * aw + x) as usize] = 255;
        }
    }
    *prep.ai_coverages.lock().unwrap() = vec![AiCoverage {
        hash: "test-ai".into(),
        width: aw,
        height: ah,
        alpha,
    }];

    let params = DevelopParams {
        masks: vec![Mask {
            name: "ai".into(),
            components: vec![MaskComponent {
                kind: ComponentKind::Ai {
                    model: "mobile_sam".into(),
                    points: vec![AiPoint {
                        x: 0.25,
                        y: 0.5,
                        positive: true,
                    }],
                    hash: "test-ai".into(),
                },
                op: MaskOp::Add,
                invert: false,
                feather: false,
            }],
            adjust: LocalAdjust {
                exposure: 2.0,
                ..Default::default()
            },
            opacity: 1.0,
            enabled: true,
        }],
        ..Default::default()
    };
    let out = pipe.render(&ctx, &prep, &params).unwrap();

    // Left band (covered) must be clearly brighter than the right band (untouched); sample away from
    // the seam to avoid the one-column boundary ramp.
    let left = mean_band(&out, 64, 4, 24, 0);
    let right = mean_band(&out, 64, 40, 60, 0);
    assert!(
        left > right + 25.0,
        "AI mask must brighten only the covered (left) half (left {left}, right {right})"
    );
}

/// Scene-referred base tone operator (ACR-matched, full Amount default): maps scene-linear ProPhoto
/// gray through the display transition to the expected ACR-default 8-bit tonality. Locks the curve
/// fit to Adobe Camera Raw's real default tone response (`base_curve_ref::ADOBE_DEFAULT_TONE_CURVE`):
/// mid-grey 0.18 → ~167 (65% sRGB, the ACR mid-grey), smooth highlight shoulder asymptoting to white.
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

    // (scene-linear input, inclusive 8-bit output bounds) — the Adobe-default tone response.
    // Mid-grey 0.18 anchors near 167 (65% sRGB). Highlights saturate to 255 (smooth shoulder).
    let cases = [
        (0.0_f32, 0.0_f64, 1.0_f64),
        (0.01, 21.0, 25.0),
        (0.05, 76.0, 80.0),
        (0.18, 165.0, 169.0),
        (0.5, 230.0, 234.0),
        (1.0, 252.0, 255.0),
        (2.0, 254.0, 255.0),
        (4.0, 254.0, 255.0),
    ];
    let mut prev = -1.0;
    for (x, lo, hi) in cases {
        let got = render_val(x);
        assert!(
            got >= lo && got <= hi,
            "base curve f({x}) → 8-bit {got}, expected [{lo},{hi}]"
        );
        // Non-decreasing (highlights saturate to 255, so equal at the very top is allowed).
        assert!(
            got >= prev,
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
        rot90: 0,
    });
    let left = center(core_pipeline::Crop {
        cx: 0.25,
        cy: 0.5,
        hw: 0.25,
        hh: 0.5,
        angle: 0.0,
        rot90: 0,
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

/// A smooth 2D diagonal gray gradient: every pixel is unique, so ANY geometry remap changes many
/// pixels (unlike a single centerline edge, which a radial remap leaves invariant on the axis).
fn ramp(w: u32, h: u32) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = 0.15 + 0.3 * (x as f32 / w as f32 + y as f32 / h as f32);
            data.extend_from_slice(&[v, v, v]);
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
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

/// Color-balance-RGB grading (binding 14): a global offset raises luminance, and full negative
/// saturation collapses a colored pixel toward neutral. Locks the grading round-trip + masks wiring.
#[test]
fn color_balance_global_offset_brightens() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    let img = solid(8, 8, [0.18, 0.18, 0.18]);
    let prep = pipe.prepare(&ctx, &img).unwrap();

    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let lifted = DevelopParams {
        cb_rgb: CbRgb {
            global: [0.15, 0.15, 0.15],
            ..CbRgb::default()
        },
        ..DevelopParams::default()
    };
    let out = pipe.render(&ctx, &prep, &lifted).unwrap();
    // Green channel must brighten (a positive grading offset raises luminance).
    assert!(
        mean_channel(&out, 1) > mean_channel(&base, 1) + 4.0,
        "global offset must brighten ({} -> {})",
        mean_channel(&base, 1),
        mean_channel(&out, 1)
    );
}

#[test]
fn color_balance_saturation_neutralizes() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    // A clearly reddish pixel (R ≫ B).
    let img = solid(8, 8, [0.5, 0.12, 0.12]);
    let prep = pipe.prepare(&ctx, &img).unwrap();

    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let spread_base = mean_channel(&base, 0) - mean_channel(&base, 2);
    assert!(
        spread_base > 20.0,
        "fixture must start saturated (r-b={spread_base})"
    );

    let desat = DevelopParams {
        cb_rgb: CbRgb {
            saturation: -1.0,
            ..CbRgb::default()
        },
        ..DevelopParams::default()
    };
    let out = pipe.render(&ctx, &prep, &desat).unwrap();
    let spread = mean_channel(&out, 0) - mean_channel(&out, 2);
    // Full negative saturation collapses chroma → channels converge (spread → near 0).
    assert!(
        spread < spread_base * 0.25,
        "saturation -1 must neutralize (r-b {spread_base} -> {spread})"
    );
}

/// Channel mixer (binding 15): swapping the green↔blue rows turns a green-dominant pixel
/// blue-dominant, and the identity default renders byte-identical to a pre-binding-15 build.
#[test]
fn channel_mix_swaps_green_blue() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    // A clearly green pixel (G ≫ B).
    let img = solid(8, 8, [0.12, 0.5, 0.12]);
    let prep = pipe.prepare(&ctx, &img).unwrap();

    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    assert!(
        mean_channel(&base, 1) > mean_channel(&base, 2) + 20.0,
        "fixture must start green-dominant (g>b)"
    );

    // Identity mixer is a no-op ⇒ byte-identical to the default render.
    let identity = DevelopParams {
        channel_mix: ChannelMix::default(),
        ..DevelopParams::default()
    };
    let ident_out = pipe.render(&ctx, &prep, &identity).unwrap();
    assert_eq!(base, ident_out, "identity mixer must not change the render");

    // Swap green↔blue: output-green takes the source blue, output-blue takes the source green.
    let swapped = DevelopParams {
        channel_mix: ChannelMix {
            green: [0.0, 0.0, 1.0],
            blue: [0.0, 1.0, 0.0],
            ..ChannelMix::default()
        },
        ..DevelopParams::default()
    };
    let out = pipe.render(&ctx, &prep, &swapped).unwrap();
    // Now blue must dominate green (the swap moved the green energy into the blue channel).
    assert!(
        mean_channel(&out, 2) > mean_channel(&out, 1) + 20.0,
        "swap G/B must make the pixel blue-dominant (g={}, b={})",
        mean_channel(&out, 1),
        mean_channel(&out, 2)
    );
}

#[test]
fn lens_distortion_remaps_nonflat_image() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    // A uniform image has nothing to warp: a geometry remap must leave it byte-identical. This is
    // also the byte-identity guard for the inactive lens path vs. strong distortion on a flat.
    let flat = solid(32, 32, [0.4, 0.4, 0.4]);
    let pf = pipe.prepare(&ctx, &flat).unwrap();
    let base_flat = pipe.render(&ctx, &pf, &DevelopParams::default()).unwrap();
    let warp_flat = pipe
        .render(
            &ctx,
            &pf,
            &DevelopParams {
                dist_k1: 100.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        base_flat, warp_flat,
        "distortion cannot change a uniform image"
    );

    // A non-flat (gradient) image DOES move under distortion — off-center pixels resample.
    let img = ramp(64, 64);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let warped = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                dist_k1: 100.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_ne!(base, warped, "distortion must remap a non-flat image");
}

#[test]
fn ca_separates_channels_on_grayscale_edge() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    // A grayscale gradient has r==g==b per pixel. Lateral CA rescales the red/blue radial sampling,
    // so off-center red and blue fetch different source locations → channels diverge.
    let img = ramp(64, 64);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let ca = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                ca_red: 100.0,
                ca_blue: -100.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_ne!(
        base, ca,
        "chromatic-aberration correction must alter the render"
    );

    let max_rb = |buf: &[u8]| {
        buf.chunks_exact(4)
            .map(|px| (px[0] as i32 - px[2] as i32).abs())
            .max()
            .unwrap_or(0)
    };
    assert!(
        max_rb(&base) <= 1,
        "grayscale edge must start with r≈b (got {})",
        max_rb(&base)
    );
    assert!(
        max_rb(&ca) > 3,
        "CA must split red vs blue off-center (got {}, base ≤ 1)",
        max_rb(&ca)
    );
}

#[test]
fn presence_clarity_texture_noop_on_flat() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    // A uniform image has zero local contrast at every scale → clarity/texture are a no-op. This is
    // the byte-identity guard for the Presence stage vs. a flat input.
    let flat = solid(32, 32, [0.4, 0.4, 0.4]);
    let pf = pipe.prepare(&ctx, &flat).unwrap();
    let base = pipe.render(&ctx, &pf, &DevelopParams::default()).unwrap();
    let presence = pipe
        .render(
            &ctx,
            &pf,
            &DevelopParams {
                clarity: 100.0,
                texture: 100.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        base, presence,
        "clarity/texture must not touch a flat image"
    );
}

#[test]
fn presence_boosts_local_contrast_on_edge() {
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

    let range = |buf: &[u8]| {
        let (mut lo, mut hi) = (255i32, 0i32);
        for px in buf.chunks_exact(4) {
            lo = lo.min(px[0] as i32);
            hi = hi.max(px[0] as i32);
        }
        hi - lo
    };
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let clar = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                clarity: 100.0,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        range(&clar) > range(&base),
        "clarity must widen local contrast across an edge ({} -> {})",
        range(&base),
        range(&clar)
    );
}

#[test]
fn dehaze_darkens_and_contrasts_flat_midtone() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);
    // Dehaze pulls the local black point down (veiling removal) — visible even on a flat midtone.
    let flat = solid(32, 32, [0.4, 0.4, 0.4]);
    let pf = pipe.prepare(&ctx, &flat).unwrap();
    let base = mean_channel(
        &pipe.render(&ctx, &pf, &DevelopParams::default()).unwrap(),
        0,
    );
    let dehazed = mean_channel(
        &pipe
            .render(
                &ctx,
                &pf,
                &DevelopParams {
                    dehaze: 100.0,
                    ..Default::default()
                },
            )
            .unwrap(),
        0,
    );
    assert!(
        dehazed < base - 3.0,
        "dehaze must darken the veiled midtone (base {base}, got {dehazed})"
    );
}
