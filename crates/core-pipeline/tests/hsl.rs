//! Golden checks for the per-hue HSL mixer: a red-band adjustment affects a red patch, and a
//! distant (blue) band does not. Skips without a GPU adapter.

use core_pipeline::{DevelopParams, DevelopPipeline, GpuContext, HslBand};
use core_raw::LinearImage;

const RED_BAND: usize = 0; // center 0°
const BLUE_BAND: usize = 5; // center 240°

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

fn spread(rgba: &[u8]) -> f64 {
    mean(rgba, 0) - mean(rgba, 2) // R - B
}

fn with_band(idx: usize, band: HslBand) -> DevelopParams {
    let mut p = DevelopParams::default();
    p.hsl[idx] = band;
    p
}

#[test]
fn hsl_mixer_is_hue_selective() {
    let ctx = match GpuContext::new() {
        Ok(c) => c,
        Err(_) => {
            eprintln!("no GPU adapter — skipping");
            return;
        }
    };
    let pipe = DevelopPipeline::new(&ctx);

    let red = solid(16, 16, [0.45, 0.12, 0.12]);
    let prep = pipe.prepare(&ctx, &red).unwrap();
    let base = pipe.render(&ctx, &prep, &DevelopParams::default()).unwrap();
    let base_spread = spread(&base);

    // Red-band saturation boost must widen the R-B spread on a red patch.
    let red_sat = pipe
        .render(
            &ctx,
            &prep,
            &with_band(
                RED_BAND,
                HslBand {
                    h: 0.0,
                    s: 80.0,
                    l: 0.0,
                },
            ),
        )
        .unwrap();
    assert!(
        spread(&red_sat) > base_spread + 3.0,
        "red-band sat+ must widen R-B spread ({base_spread} -> {})",
        spread(&red_sat)
    );

    // A blue-band adjustment must NOT affect a pure-red patch.
    let blue_sat = pipe
        .render(
            &ctx,
            &prep,
            &with_band(
                BLUE_BAND,
                HslBand {
                    h: 0.0,
                    s: 80.0,
                    l: 0.0,
                },
            ),
        )
        .unwrap();
    assert!(
        (spread(&blue_sat) - base_spread).abs() < 2.0,
        "blue-band must leave a red patch unchanged ({base_spread} -> {})",
        spread(&blue_sat)
    );

    // Red-band luminance- must darken the red patch.
    let red_dark = pipe
        .render(
            &ctx,
            &prep,
            &with_band(
                RED_BAND,
                HslBand {
                    h: 0.0,
                    s: 0.0,
                    l: -80.0,
                },
            ),
        )
        .unwrap();
    assert!(
        mean(&red_dark, 0) < mean(&base, 0) - 5.0,
        "red-band lum- must darken R ({} -> {})",
        mean(&base, 0),
        mean(&red_dark, 0)
    );
}
