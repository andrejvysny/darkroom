//! Lazy mask/readback allocation + nominal byte accounting of `PreparedImage`.
//!
//! Proves (1) a prepare allocates no full-res mask/scratch/readback, (2) GPU-resident renders
//! without masks keep them at 0, (3) the first masked render allocates them and its pixels equal
//! a fresh image rendered masked directly, (4) `estimate_prepare_bytes` equals the resident total
//! right after `prepare`. Synthetic images; GPU-gated via `common::gpu()` (the arithmetic test is
//! pure and always runs).

mod common;

use core_pipeline::backend::{
    estimate_mask_bytes, estimate_prepare_bytes, estimate_readback_bytes,
};
use core_pipeline::{
    ComponentKind, DevelopParams, DevelopPipeline, LocalAdjust, Mask, MaskComponent, MaskOp,
    ViewParams,
};
use core_raw::LinearImage;

fn ramp(w: u32, h: u32) -> LinearImage {
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            data.push(x as f32 / (w - 1).max(1) as f32);
            data.push(y as f32 / (h - 1).max(1) as f32);
            data.push((x + y) as f32 / (w + h - 2).max(1) as f32);
        }
    }
    LinearImage {
        width: w,
        height: h,
        data,
    }
}

fn luma_mask() -> Mask {
    Mask {
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
    }
}

fn masked() -> DevelopParams {
    DevelopParams {
        masks: vec![luma_mask()],
        ..Default::default()
    }
}

fn target(ctx: &core_pipeline::GpuContext, w: u32, h: u32) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("resources-test-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

#[test]
fn masks_and_readback_are_lazy_and_pixels_match_eager_baseline() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let (w, h) = (64u32, 48u32);
    let view = ViewParams::full(w, h);
    let t = target(&ctx, w, h);

    let a = pipe.prepare(&ctx, &ramp(w, h)).unwrap();
    let r0 = a.resident_bytes();
    assert_eq!(
        (r0.masks, r0.scratch, r0.readback, r0.view_target),
        (0, 0, 0, 0)
    );

    // GPU-resident render without masks: nothing lazy is created.
    pipe.render_view_into_texture(&ctx, &a, &DevelopParams::default(), &view, None, &t)
        .unwrap();
    let plain = pipe.read_back_texture(&ctx, &t).unwrap();
    let r1 = a.resident_bytes();
    assert_eq!((r1.masks, r1.scratch, r1.readback), (0, 0, 0));
    assert_eq!(r1, r0);

    // Enabling a mask allocates the full-res array + scratch (still no readback).
    pipe.render_view_into_texture(&ctx, &a, &masked(), &view, None, &t)
        .unwrap();
    let after = pipe.read_back_texture(&ctx, &t).unwrap();
    let r2 = a.resident_bytes();
    assert_eq!(r2.masks, 64 * 48 * 2 * 16);
    assert_eq!(r2.scratch, 64 * 48 * 2 * 3);
    assert_eq!(r2.readback, 0);
    assert_eq!(r2.masks + r2.scratch, estimate_mask_bytes(w, h));
    assert_ne!(plain, after, "mask must change pixels");

    // Fresh image, masked from the first render (mask allocation happens in that same call).
    let b = pipe.prepare(&ctx, &ramp(w, h)).unwrap();
    pipe.render_view_into_texture(&ctx, &b, &masked(), &view, None, &t)
        .unwrap();
    let direct = pipe.read_back_texture(&ctx, &t).unwrap();
    assert_eq!(after, direct, "lazy mask allocation must not change pixels");

    // CPU-readback path (render_view / render) creates the readback buffer on first use only.
    let via_cpu = pipe.render_view(&ctx, &b, &masked(), &view).unwrap();
    assert_eq!(via_cpu, direct);
    assert_eq!(b.resident_bytes().readback, estimate_readback_bytes(w, h));
    let via_render = pipe.render(&ctx, &b, &masked()).unwrap();
    assert_eq!(via_render, direct);

    // Dropping masks again keeps the allocation (no shrink) and restores the unmasked pixels.
    pipe.render_view_into_texture(&ctx, &a, &DevelopParams::default(), &view, None, &t)
        .unwrap();
    assert_eq!(pipe.read_back_texture(&ctx, &t).unwrap(), plain);
    assert_eq!(a.resident_bytes().masks, r2.masks);
}

#[test]
fn mask_overlay_without_masks_allocates_masks() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let a = pipe.prepare(&ctx, &ramp(64, 48)).unwrap();
    let mut view = ViewParams::full(64, 48);
    view.overlay_layer = 0;
    let t = target(&ctx, 64, 48);
    pipe.render_view_into_texture(&ctx, &a, &DevelopParams::default(), &view, None, &t)
        .unwrap();
    assert!(a.resident_bytes().masks > 0, "overlay samples mask_tex");
}

#[test]
fn estimate_equals_resident_total_after_prepare() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let p = pipe.prepare(&ctx, &ramp(64, 48)).unwrap();
    assert_eq!(p.resident_bytes().total(), estimate_prepare_bytes(64, 48));
}

// Pure arithmetic: no device, no allocation.
#[test]
fn estimate_arithmetic_for_large_inputs() {
    // Eager overhead independent of image size (uniforms, LUTs, placeholder, ai_tex).
    let fixed = estimate_prepare_bytes(64, 48) - (65_520 + 12_288);
    assert!(fixed >= 1_048_576, "ai_tex alone is 1 MiB, got {fixed}");
    for (w, h, input, out, mask, rb) in [
        (
            6960u32,
            4640u32,
            688_943_504u64,
            129_177_600u64,
            1_227_187_200u64,
            129_474_560u64,
        ),
        (
            9504,
            6336,
            1_284_634_256,
            240_869_376,
            2_288_259_072,
            241_680_384,
        ),
        (
            20000,
            8000,
            3_413_329_456,
            640_000_000,
            6_080_000_000,
            641_024_000,
        ),
    ] {
        assert_eq!(estimate_prepare_bytes(w, h), input + out + fixed, "{w}x{h}");
        assert_eq!(estimate_mask_bytes(w, h), mask, "{w}x{h}");
        assert_eq!(estimate_readback_bytes(w, h), rb, "{w}x{h}");
    }
}
