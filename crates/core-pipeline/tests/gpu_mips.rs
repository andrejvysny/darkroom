//! GPU mip chain (compute shader) vs the CPU box-filter chain. GPU-gated via `common::gpu()`.

mod common;

use core_pipeline::{DevelopPipeline, LinearImage};

fn ramp(w: u32, h: u32, scale: f32) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(scale * x as f32 / (w - 1).max(1) as f32);
            data.push(scale * y as f32 / (h - 1).max(1) as f32);
            data.push(scale * ((x * 7 + y * 13) % 31) as f32 / 31.0);
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

fn compare(name: &str, img: &LinearImage) {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let g = pipe.debug_mip_levels(&ctx, img, true).expect("gpu mips");
    let c = pipe.debug_mip_levels(&ctx, img, false).expect("cpu mips");
    assert_eq!(g.len(), c.len(), "{name}: level count");
    assert!(
        g[0].iter()
            .zip(&c[0])
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "{name}: level 0"
    );
    for (l, (gl, cl)) in g.iter().zip(&c).enumerate() {
        assert_eq!(gl.len(), cl.len(), "{name}: level {l} len");
        for (i, (a, b)) in gl.iter().zip(cl).enumerate() {
            assert!(
                (a - b).abs() <= 1e-6 * b.abs().max(1.0),
                "{name}: level {l} idx {i}: gpu {a} cpu {b}"
            );
        }
    }
}

#[test]
fn ramps_match_cpu() {
    for (w, h) in [(64, 48), (33, 17), (7, 3), (1, 9), (6, 5), (4097, 3)] {
        compare(&format!("{w}x{h}"), &ramp(w, h, 1.0));
        compare(&format!("{w}x{h} hdr"), &ramp(w, h, 8.0));
    }
}

#[test]
fn overflowing_hdr_sum_matches_cpu() {
    // Four ~3e38 samples overflow the f32 sum; the average is still representable.
    let (w, h) = (8u32, 6u32);
    let data = (0..w * h * 3)
        .map(|i| 3.0e38 * (0.9 + 0.1 * (i % 5) as f32 / 4.0))
        .collect();
    compare(
        "overflow even",
        &LinearImage {
            width: w,
            height: h,
            data,
        },
    );
    let (w, h) = (7u32, 5u32);
    let data = (0..w * h * 3)
        .map(|i| 3.0e38 * (0.9 + 0.1 * (i % 5) as f32 / 4.0))
        .collect();
    compare(
        "overflow odd",
        &LinearImage {
            width: w,
            height: h,
            data,
        },
    );
}
