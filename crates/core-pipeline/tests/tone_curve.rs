//! Golden checks that the tone-curve LUT stage is wired into the GPU pipeline correctly:
//! an identity curve is a no-op, and a brightening curve lifts mid-gray. Skips without a GPU.

use core_pipeline::{CurvePoint, DevelopParams, DevelopPipeline, GpuContext, ToneCurve};
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

fn mean(rgba: &[u8], ch: usize) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for px in rgba.chunks_exact(4) {
        sum += px[ch] as u64;
        n += 1;
    }
    sum as f64 / n as f64
}

fn pt(x: f32, y: f32) -> CurvePoint {
    CurvePoint { x, y }
}

#[test]
fn tone_curve_applies_in_pipeline() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    // ~0.18 linear ≈ mid-gray after sRGB OETF — lands near the middle of the curve.
    let gray = solid(16, 16, [0.18, 0.18, 0.18]);
    let prep = pipe.prepare(&ctx, &gray).unwrap();

    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let l_base = mean(&base, 0);

    // Identity curve must not change the output.
    let identity = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                tone_curve: ToneCurve {
                    rgb: vec![pt(0.0, 0.0), pt(0.5, 0.5), pt(1.0, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        (mean(&identity, 0) - l_base).abs() < 2.0,
        "identity curve must be a no-op (base {l_base}, got {})",
        mean(&identity, 0)
    );

    // Brightening curve (lift midtones) must raise the mean.
    let bright = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                tone_curve: ToneCurve {
                    rgb: vec![pt(0.0, 0.0), pt(0.5, 0.72), pt(1.0, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        mean(&bright, 0) > l_base + 10.0,
        "brightening curve must lift mid-gray (base {l_base}, got {})",
        mean(&bright, 0)
    );

    // Per-channel: a red-only lift raises R but leaves B unchanged.
    let red_lift = pipe
        .render(
            &ctx,
            &prep,
            &DevelopParams {
                tone_curve: ToneCurve {
                    r: vec![pt(0.0, 0.0), pt(0.5, 0.8), pt(1.0, 1.0)],
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        mean(&red_lift, 0) > l_base + 8.0,
        "red curve must lift R (base {l_base}, got {})",
        mean(&red_lift, 0)
    );
    assert!(
        (mean(&red_lift, 2) - l_base).abs() < 2.0,
        "red curve must leave B unchanged (base {l_base}, got {})",
        mean(&red_lift, 2)
    );
}
