//! Export uses native selected pixels, including swapped dimensions after a quarter-turn.
mod common;

use core_pipeline::{crop_rgba8, Crop, DevelopParams, DevelopPipeline, LinearImage};

#[test]
fn native_crop_and_rotation_preserve_source_pixels() {
    let Some(ctx) = common::gpu() else { return };
    let pipeline = DevelopPipeline::new(&ctx);
    let mut data = Vec::new();
    for y in 0..24 {
        for x in 0..32 {
            data.extend_from_slice(&[0.02 + x as f32 / 80.0, 0.02 + y as f32 / 60.0, 0.1]);
        }
    }
    let image = LinearImage {
        width: 32,
        height: 24,
        data,
    };
    let mut params = DevelopParams::default();
    let (full, w, h) = pipeline
        .render_cropped_with_ai_coverages(&ctx, &image, &params, &[])
        .unwrap();
    assert_eq!((w, h), (32, 24));
    params.crop.hw = 0.25;
    params.crop.hh = 0.25;
    let (crop, w, h) = pipeline
        .render_cropped_with_ai_coverages(&ctx, &image, &params, &[])
        .unwrap();
    assert_eq!(
        (w, h),
        (16, 12),
        "cropping must not upscale to the source dimensions"
    );
    let expected = crop_rgba8(&full, 32, 8, 6, 16, 12);
    assert!(crop.iter().zip(expected).all(|(a, b)| a.abs_diff(b) <= 1));

    params.crop = Crop {
        rot90: 1,
        ..Default::default()
    };
    let (rotated, w, h) = pipeline
        .render_cropped_with_ai_coverages(&ctx, &image, &params, &[])
        .unwrap();
    assert_eq!(
        (w, h),
        (24, 32),
        "rotation must swap axes without discarding resolution"
    );
    for y in 0..32usize {
        for x in 0..24usize {
            let dst = (y * 24 + x) * 4;
            let src = ((23 - x) * 32 + y) * 4;
            for channel in 0..4 {
                assert!(rotated[dst + channel].abs_diff(full[src + channel]) <= 1);
            }
        }
    }
}
