//! Mask packing + GPU guards:
//! - CPU: `to_mask_buffer` packs ENABLED masks densely (layers 0..count), skips disabled, caps at
//!   MASK_CAP, and normalizes scalar deltas like the global uniform.
//! - GPU: a render with ZERO-coverage mask alpha is byte-identical to one with no masks (the no-op
//!   guard for the develop.wgsl refactor + new bindings). NOTE: masks are fully wired now — the
//!   parametric/radial/brush/range pre-pass writes the alpha layers (`mask.rs`/`mask_prepass.wgsl`);
//!   other tests here exercise that real coverage changing pixels.

use core_pipeline::params::MaskBufferUniform;
use core_pipeline::{
    ComponentKind, DevelopParams, DevelopPipeline, GpuContext, LocalAdjust, Mask, MaskComponent,
    MaskOp, MASK_CAP,
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
    let mut p = DevelopParams::default();
    p.masks = (0..MASK_CAP + 4)
        .map(|_| brush_mask(LocalAdjust::default(), true))
        .collect();
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
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
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
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
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

#[test]
fn empty_brush_mask_is_inert() {
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
