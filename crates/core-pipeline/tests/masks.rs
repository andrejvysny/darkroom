//! Mask packing + GPU guards:
//! - CPU: `to_mask_buffer` packs ENABLED masks densely (layers 0..count), skips disabled, caps at
//!   MASK_CAP, and normalizes scalar deltas like the global uniform.
//! - GPU: a render with ZERO-coverage mask alpha is byte-identical to one with no masks (the no-op
//!   guard for the develop.wgsl refactor + new bindings). NOTE: masks are fully wired now — the
//!   parametric/radial/brush/range pre-pass writes the alpha layers (`mask.rs`/`mask_prepass.wgsl`);
//!   other tests here exercise that real coverage changing pixels.

mod common;

use core_pipeline::params::MaskBufferUniform;
use core_pipeline::{
    ComponentKind, DevelopParams, DevelopPipeline, LocalAdjust, Mask, MaskComponent, MaskOp,
    MASK_CAP,
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

fn brush_mask(adjust: LocalAdjust, enabled: bool) -> Mask {
    Mask {
        name: "m".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Brush { strokes: vec![] },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }],
        adjust,
        opacity: 1.0,
        enabled,
    }
}

#[test]
fn to_mask_buffer_packs_enabled_only() {
    let mut p = DevelopParams::default();
    assert_eq!(p.to_mask_buffer().count, 0, "no masks => count 0");

    p.masks = vec![
        brush_mask(
            LocalAdjust {
                exposure: 1.0,
                ..Default::default()
            },
            true,
        ),
        brush_mask(LocalAdjust::default(), false), // disabled => skipped
        brush_mask(
            LocalAdjust {
                contrast: 50.0,
                ..Default::default()
            },
            true,
        ),
    ];
    let buf = p.to_mask_buffer();
    assert_eq!(buf.count, 2, "two enabled masks packed");
    assert_eq!(
        buf.masks[0].exposure, 1.0,
        "exposure delta passes raw (stops)"
    );
    assert!((buf.masks[0].opacity - 1.0).abs() < 1e-6, "opacity carried");
    // Densely packed: the disabled mask is dropped, so the contrast mask is at layer 1.
    assert!(
        (buf.masks[1].contrast - 0.5).abs() < 1e-6,
        "contrast normalized /100 (got {})",
        buf.masks[1].contrast
    );
    assert_eq!(buf.masks[1].enabled, 1.0, "packed entries flagged enabled");
}

#[test]
fn to_mask_buffer_caps_at_mask_cap() {
    let p = DevelopParams {
        masks: (0..MASK_CAP + 4)
            .map(|_| brush_mask(LocalAdjust::default(), true))
            .collect(),
        ..Default::default()
    };
    assert_eq!(
        p.to_mask_buffer().count as usize,
        MASK_CAP,
        "count clamps to MASK_CAP"
    );
    // Buffer size is fixed regardless of mask count.
    assert_eq!(std::mem::size_of::<MaskBufferUniform>(), 16 + MASK_CAP * 48);
}

/// Two-tone image: left half dark, right half bright (for range-mask tests).
fn split_tone(w: u32, h: u32, left: [f32; 3], right: [f32; 3]) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for _ in 0..h {
        for x in 0..w {
            data.extend_from_slice(if x < w / 2 { &left } else { &right });
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

#[test]
fn luminance_range_selects_brights_only() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = split_tone(64, 16, [0.04, 0.04, 0.04], [0.5, 0.5, 0.5]);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();

    // Select display luma in [0.5, 1.0] (the bright right half), darken it.
    let mask = Mask {
        name: "luma".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::LuminanceRange {
                lo: 0.5,
                hi: 1.0,
                feather: 0.05,
            },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }],
        adjust: LocalAdjust {
            exposure: -2.0,
            ..Default::default()
        },
        opacity: 1.0,
        enabled: true,
    };
    let out = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                masks: vec![mask],
                ..Default::default()
            },
        )
        .unwrap();

    let at = |buf: &[u8], col: usize| buf[(8 * 64 + col) * 4] as i32;
    // Right (bright) half must darken; left (dark) half unchanged.
    assert!(
        at(&out, 48) < at(&base, 48) - 10,
        "bright half must darken ({} -> {})",
        at(&base, 48),
        at(&out, 48)
    );
    assert!(
        (at(&out, 8) - at(&base, 8)).abs() <= 2,
        "dark half must be ~unchanged"
    );
}

#[test]
fn component_intersect_narrows_coverage() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let img = split_tone(64, 16, [0.04, 0.04, 0.04], [0.5, 0.5, 0.5]);
    let prep = pipe.prepare(&ctx, &img).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();

    // Full-coverage radial (Add) ∩ luminance-range(brights) ⇒ only the bright half is affected.
    let mask = Mask {
        name: "combo".into(),
        components: vec![
            MaskComponent {
                kind: ComponentKind::Radial {
                    center: [0.5, 0.5],
                    radius: [10.0, 10.0],
                    angle: 0.0,
                    feather: 0.0,
                },
                op: MaskOp::Add,
                invert: false,
                feather: false,
            },
            MaskComponent {
                kind: ComponentKind::LuminanceRange {
                    lo: 0.5,
                    hi: 1.0,
                    feather: 0.05,
                },
                op: MaskOp::Intersect,
                invert: false,
                feather: false,
            },
        ],
        adjust: LocalAdjust {
            exposure: -2.0,
            ..Default::default()
        },
        opacity: 1.0,
        enabled: true,
    };
    let out = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                masks: vec![mask],
                ..Default::default()
            },
        )
        .unwrap();

    let at = |buf: &[u8], col: usize| buf[(8 * 64 + col) * 4] as i32;
    assert!(
        at(&out, 48) < at(&base, 48) - 10,
        "intersect: bright half affected"
    );
    assert!(
        (at(&out, 8) - at(&base, 8)).abs() <= 2,
        "intersect: dark half excluded"
    );
}

#[test]
fn subtract_or_intersect_first_component_still_covers() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let gray = solid(16, 16, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();

    // A single full-coverage component whose op is Subtract/Intersect must NOT be silently inert:
    // the pre-pass seeds running coverage from the first component regardless of its op (regression
    // guard for the zero-alpha trap where a Subtract/Intersect-first mask stayed at alpha 0 forever).
    for op in [MaskOp::Subtract, MaskOp::Intersect] {
        let mask = Mask {
            name: "first-op".into(),
            components: vec![MaskComponent {
                kind: ComponentKind::Radial {
                    center: [0.5, 0.5],
                    radius: [10.0, 10.0], // rr≈0 across [0,1] ⇒ coverage 1 everywhere
                    angle: 0.0,
                    feather: 0.0,
                },
                op,
                invert: false,
                feather: false,
            }],
            adjust: LocalAdjust {
                exposure: 1.0,
                ..Default::default()
            },
            opacity: 1.0,
            enabled: true,
        };
        let out = pipe
            .render(
                &ctx,
                &prep,
                &DevelopParams {
                    masks: vec![mask],
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            mean_channel(&out, 0) > mean_channel(&base, 0) + 10.0,
            "{op:?}-first component must still cover (brighten vs base)"
        );
    }
}

/// A radial component large enough to cover the whole image with alpha≈1 (full coverage).
fn full_coverage_mask(adjust: LocalAdjust) -> Mask {
    Mask {
        name: "full".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Radial {
                center: [0.5, 0.5],
                radius: [10.0, 10.0], // rr≈0 across [0,1] ⇒ coverage 1 everywhere
                angle: 0.0,
                feather: 0.0,
            },
            op: MaskOp::Add,
            invert: false,
            feather: false,
        }],
        adjust,
        opacity: 1.0,
        enabled: true,
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
fn full_coverage_mask_matches_global() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let gray = solid(16, 16, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();

    // Global exposure +1.0.
    let global = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                exposure: 1.0,
                ..Default::default()
            },
        )
        .unwrap();

    // Same exposure delivered via a full-coverage mask on a neutral base.
    let masked = DevelopParams {
        masks: vec![full_coverage_mask(LocalAdjust {
            exposure: 1.0,
            ..Default::default()
        })],
        ..Default::default()
    };
    let out = pipe.render(&ctx, &prep, &masked).unwrap();

    // Should match closely (mask alpha≈1; tiny edge differences from R16Float coverage allowed).
    let dg = mean_channel(&global, 0);
    let dm = mean_channel(&out, 0);
    assert!(
        (dg - dm).abs() < 2.0,
        "full-coverage mask exposure must match global (global {dg}, masked {dm})"
    );
    // And it must differ clearly from the unadjusted base.
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    assert!(
        dm > mean_channel(&base, 0) + 10.0,
        "masked exposure must brighten vs base"
    );
}

fn assert_pixels_close(actual: &[u8], expected: &[u8], tolerance: u8) {
    assert_eq!(actual.len(), expected.len());
    let worst = actual
        .iter()
        .zip(expected)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= tolerance,
        "maximum pixel difference {worst} exceeds {tolerance}"
    );
}

#[test]
fn neutral_overlapping_masks_preserve_all_prior_adjustments() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe
        .prepare(&ctx, &solid(16, 16, [0.08, 0.12, 0.06]))
        .unwrap();
    let p = DevelopParams {
        exposure: 0.3,
        contrast: 12.0,
        shadows: 20.0,
        masks: vec![full_coverage_mask(LocalAdjust {
            exposure: 0.7,
            temp: 20.0,
            saturation: 15.0,
            contrast: 25.0,
            blacks: 10.0,
            whites: -10.0,
            ..Default::default()
        })],
        ..Default::default()
    };
    let expected = pipe.render(&ctx, &prep, &p).unwrap();
    for index in 0..=p.masks.len() {
        for opacity in [0.0, 0.4, 1.0] {
            let mut with_neutral = p.clone();
            let mut neutral = full_coverage_mask(LocalAdjust::default());
            neutral.opacity = opacity;
            with_neutral.masks.insert(index, neutral);
            assert_eq!(pipe.render(&ctx, &prep, &with_neutral).unwrap(), expected);
        }
    }
    let mut with_disabled = p.clone();
    let mut disabled = full_coverage_mask(LocalAdjust {
        exposure: 5.0,
        contrast: 100.0,
        ..Default::default()
    });
    disabled.enabled = false;
    with_disabled.masks.push(disabled);
    assert_eq!(pipe.render(&ctx, &prep, &with_disabled).unwrap(), expected);
}

#[test]
fn overlapping_exposures_compose_in_linear_light_with_opacity() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe
        .prepare(&ctx, &solid(16, 16, [0.10, 0.08, 0.06]))
        .unwrap();
    // Analytic reference: a +1 EV mask doubles the pixel, and a half-opacity +1 EV
    // mask multiplies it by (1-0.5)+0.5*2 = 1.5. Together 0.10 -> 0.20 -> 0.30.
    let mut half = full_coverage_mask(LocalAdjust {
        exposure: 1.0,
        ..Default::default()
    });
    half.opacity = 0.5;
    let first = full_coverage_mask(LocalAdjust {
        exposure: 1.0,
        ..Default::default()
    });
    let expected = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                exposure: 3.0_f32.log2(),
                ..Default::default()
            },
        )
        .unwrap();
    for masks in [vec![first.clone(), half.clone()], vec![half, first]] {
        let p = DevelopParams {
            masks,
            ..Default::default()
        };
        assert_pixels_close(&pipe.render(&ctx, &prep, &p).unwrap(), &expected, 1);
    }
}

#[test]
fn display_masks_compose_without_erasing_scene_masks() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe
        .prepare(&ctx, &solid(16, 16, [0.10, 0.08, 0.06]))
        .unwrap();
    let scene = full_coverage_mask(LocalAdjust {
        exposure: 1.0,
        temp: 15.0,
        saturation: 20.0,
        ..Default::default()
    });
    let scene_params = DevelopParams {
        masks: vec![scene.clone()],
        ..Default::default()
    };
    let base = pipe.render(&ctx, &prep, &scene_params).unwrap();
    // Display reference is evaluated independently on the developed pixel (8-bit quantization
    // permits <=2 LSB). Contrast, blacks, whites operate sequentially after the tone transition.
    let controls = [(30.0, 10.0, -15.0, 0.5), (-20.0, -10.0, 25.0, 1.0)];
    for reversed in [false, true] {
        let order = if reversed {
            [controls[1], controls[0]]
        } else {
            controls
        };
        let mut p = scene_params.clone();
        let mut expected = base.clone();
        for &(contrast, blacks, whites, opacity) in &order {
            let mut mask = full_coverage_mask(LocalAdjust {
                contrast,
                blacks,
                whites,
                ..Default::default()
            });
            mask.opacity = opacity;
            p.masks.push(mask);
        }
        for px in expected.chunks_exact_mut(4) {
            for channel in &mut px[..3] {
                let mut value = *channel as f32 / 255.0;
                for (contrast, blacks, whites, opacity) in order {
                    let contrasted = (value - 0.5) * (1.0 + contrast / 100.0) + 0.5;
                    let white = contrasted * (1.0 + whites / 100.0 * 0.2);
                    let adjusted = 1.0 - (1.0 - white) * (1.0 - blacks / 100.0 * 0.2);
                    value += opacity * (adjusted - value);
                }
                *channel = (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
        assert_pixels_close(&pipe.render(&ctx, &prep, &p).unwrap(), &expected, 2);
        // Swapping cross-domain insertion order cannot erase either domain's result.
        p.masks.rotate_left(1);
        assert_pixels_close(&pipe.render(&ctx, &prep, &p).unwrap(), &expected, 2);
    }
}

#[test]
fn disjoint_masks_leave_the_other_regions_adjustment_unchanged() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe.prepare(&ctx, &solid(64, 16, [0.1, 0.1, 0.1])).unwrap();
    let local = |cx, ev| {
        let mut mask = full_coverage_mask(LocalAdjust {
            exposure: ev,
            ..Default::default()
        });
        mask.components[0].kind = ComponentKind::Radial {
            center: [cx, 0.5],
            radius: [0.20, 2.0],
            angle: 0.0,
            feather: 0.0,
        };
        mask
    };
    let left = local(0.25, 1.0);
    let right = local(0.75, -1.0);
    let render = |masks| {
        pipe.render(
            &ctx,
            &prep,
            &DevelopParams {
                masks,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let left_only = render(vec![left.clone()]);
    let right_only = render(vec![right.clone()]);
    let both = render(vec![left.clone(), right.clone()]);
    assert_eq!(
        &both[(8 * 64 + 16) * 4..(8 * 64 + 16) * 4 + 4],
        &left_only[(8 * 64 + 16) * 4..(8 * 64 + 16) * 4 + 4]
    );
    assert_eq!(
        &both[(8 * 64 + 48) * 4..(8 * 64 + 48) * 4 + 4],
        &right_only[(8 * 64 + 48) * 4..(8 * 64 + 48) * 4 + 4]
    );
    assert_eq!(both, render(vec![right, left]));
}

#[test]
fn partially_overlapping_masks_only_compound_inside_the_overlap() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe
        .prepare(&ctx, &solid(100, 16, [0.05, 0.05, 0.05]))
        .unwrap();
    let local = |cx| {
        let mut mask = full_coverage_mask(LocalAdjust {
            exposure: 1.0,
            ..Default::default()
        });
        mask.components[0].kind = ComponentKind::Radial {
            center: [cx, 0.5],
            radius: [0.3, 2.0],
            angle: 0.0,
            feather: 0.0,
        };
        mask
    };
    let left = local(0.35);
    let right = local(0.65);
    let both = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                masks: vec![left, right],
                ..Default::default()
            },
        )
        .unwrap();
    for (x, ev) in [(1, 0.0), (15, 1.0), (50, 2.0), (85, 1.0), (98, 0.0)] {
        let reference = pipe
            .render(
                &ctx,
                &prep,
                &DevelopParams {
                    exposure: ev,
                    ..Default::default()
                },
            )
            .unwrap();
        let index = (8 * 100 + x) * 4;
        assert_pixels_close(&both[index..index + 4], &reference[index..index + 4], 1);
    }
}

#[test]
fn ordered_temperature_and_saturation_match_linear_reference() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let rgb = [0.12, 0.08, 0.05];
    let prep = pipe.prepare(&ctx, &solid(16, 16, rgb)).unwrap();
    let warm = full_coverage_mask(LocalAdjust {
        temp: 40.0,
        tint: 10.0,
        ..Default::default()
    });
    let saturation = full_coverage_mask(LocalAdjust {
        saturation: -60.0,
        ..Default::default()
    });
    for masks in [
        vec![warm.clone(), saturation.clone()],
        vec![saturation, warm],
    ] {
        let p = DevelopParams {
            masks,
            ..Default::default()
        };
        // Reference uses the existing packed local-WB gains; it evaluates only the two relevant
        // linear operations, then lets the unchanged display pipeline render that known input.
        // The Rec.709 luma weights here preserve current processing semantics; working-space
        // luminance calibration is a separate compatibility change.
        let mut expected = rgb;
        for mask in p.to_mask_buffer().masks.iter().take(2) {
            let balanced = std::array::from_fn::<_, 3, _>(|c| expected[c] * mask.wb_gain[c]);
            let luma = balanced[0] * 0.2126 + balanced[1] * 0.7152 + balanced[2] * 0.0722;
            expected = balanced.map(|c| (luma + (c - luma) * (1.0 + mask.saturation)).max(0.0));
        }
        let reference = pipe
            .render_once(&ctx, &solid(16, 16, expected), &DevelopParams::default())
            .unwrap();
        assert_pixels_close(&pipe.render(&ctx, &prep, &p).unwrap(), &reference, 1);
    }
}

#[test]
fn empty_brush_mask_is_inert() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let gray = solid(16, 16, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();

    // A brush mask with NO strokes bakes zero coverage ⇒ no effect.
    let masked = DevelopParams {
        masks: vec![brush_mask(
            LocalAdjust {
                exposure: 3.0,
                ..Default::default()
            },
            true,
        )],
        ..Default::default()
    };
    let out = pipe.render(&ctx, &prep, &masked).unwrap();
    assert_eq!(base, out, "empty brush mask must be inert");
}

#[test]
fn brush_stroke_brightens_locally() {
    use core_pipeline::BrushStroke;
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let gray = solid(64, 64, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();

    // A brush dab over the center-left with a bright exposure delta.
    let mask = Mask {
        name: "brush".into(),
        components: vec![MaskComponent {
            kind: ComponentKind::Brush {
                strokes: vec![BrushStroke {
                    points: vec![[0.25, 0.5]],
                    size: 0.2,
                    hardness: 0.5,
                    flow: 1.0,
                    opacity: 1.0,
                    is_erase: false,
                }],
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
    };
    let out = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                masks: vec![mask],
                ..Default::default()
            },
        )
        .unwrap();

    // Pixel under the dab (col 16, row 32) must be brighter; a far corner (col 60) unchanged.
    let at = |buf: &[u8], col: usize, row: usize| buf[(row * 64 + col) * 4] as i32;
    assert!(
        at(&out, 16, 32) > at(&base, 16, 32) + 10,
        "under-brush pixel must brighten ({} -> {})",
        at(&base, 16, 32),
        at(&out, 16, 32)
    );
    assert!(
        (at(&out, 60, 32) - at(&base, 60, 32)).abs() <= 2,
        "far pixel must be ~unchanged"
    );
}

/// Mask-layer caching (A5): a cache HIT (geometry unchanged) must still apply the per-frame scalar
/// adjustments — it only skips re-baking coverage — and a geometry change must INVALIDATE the layer.
#[test]
fn mask_cache_applies_scalars_and_invalidates_on_geometry() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let gray = solid(64, 32, [0.2, 0.2, 0.2]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();

    let radial = |cx: f32, exposure: f32| DevelopParams {
        masks: vec![Mask {
            name: "m".into(),
            components: vec![MaskComponent {
                kind: ComponentKind::Radial {
                    center: [cx, 0.5],
                    radius: [0.18, 0.6],
                    angle: 0.0,
                    feather: 0.0,
                },
                op: MaskOp::Add,
                invert: false,
                feather: false,
            }],
            adjust: LocalAdjust {
                exposure,
                ..Default::default()
            },
            opacity: 1.0,
            enabled: true,
        }],
        ..Default::default()
    };
    let at = |buf: &[u8], col: usize| buf[(16 * 64 + col) * 4] as i32;

    // 1) Left radial, +2 EV → left brightens (cold compute).
    let bright = pipe.render(&ctx, &prep, &radial(0.2, 2.0)).unwrap();
    assert!(at(&bright, 12) > at(&base, 12) + 10, "left must brighten");
    // 2) SAME geometry, 0 EV (layer cache HIT) → left returns to base: the scalar is applied fresh,
    //    not frozen by the cache.
    let flat = pipe.render(&ctx, &prep, &radial(0.2, 0.0)).unwrap();
    assert!(
        (at(&flat, 12) - at(&base, 12)).abs() <= 3,
        "cache hit must not freeze the scalar ({} vs base {})",
        at(&flat, 12),
        at(&base, 12)
    );
    // 3) Geometry change (right radial, +2 EV) → right brightens, left back to base (invalidation).
    let right = pipe.render(&ctx, &prep, &radial(0.8, 2.0)).unwrap();
    assert!(
        at(&right, 52) > at(&base, 52) + 10,
        "right must brighten after geometry change"
    );
    assert!(
        (at(&right, 12) - at(&base, 12)).abs() <= 3,
        "left must no longer be affected after the mask moved"
    );
}
