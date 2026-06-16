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

/// Scene-referred highlight headroom: linear values >1.0 must NOT hard-clip to pure white. The soft
/// rolloff keeps them below 255 and keeps brighter inputs brighter (so a Highlights pull can recover
/// detail). Regression guard for removing the old `clamp(lin,0,1)` pre-OETF.
#[test]
fn highlights_above_one_roll_off_not_clip() {
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
        mean_channel(&out, 0)
    };

    let at_one = render_val(1.0);
    let at_onefive = render_val(1.5);
    let at_two = render_val(2.0);

    // A linear 1.0 must roll off below pure white (the old hard clamp would make it exactly 255).
    assert!(
        at_one < 252.0,
        "linear 1.0 must roll off below 255 (got {at_one})"
    );
    // Headroom above 1.0 is preserved as distinguishable brightness (detail to recover), not all-255.
    assert!(
        at_onefive > at_one + 3.0,
        "linear 1.5 must read brighter than 1.0 ({at_one} -> {at_onefive})"
    );
    assert!(
        at_two >= at_onefive - 1.0,
        "monotonic into the shoulder ({at_onefive} -> {at_two})"
    );
}
