//! GPU-resident develop path: `render_view_into_texture` + `read_back_texture`.
//!
//! Proves (1) the texture-target path is byte-identical to the CPU-readback path
//! (`render_view`), (2) back-to-back submissions without waiting each keep the uniforms they
//! were submitted with (wgpu applies `queue.write_buffer` at the next `submit`), and (3) a
//! wrong target (size/format/usage) is a typed error, never a panic.
//! Synthetic images only — GPU-gated via `common::gpu()` like `param_effects.rs`.

mod common;

use core_pipeline::{DevelopParams, DevelopPipeline, ViewParams};
use core_raw::LinearImage;

/// Smooth gradient with all channels varying, so develop output is non-flat and any uniform
/// mix-up changes bytes.
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

/// Active viewport view: whole crop window into an `out_w × out_h` target, overlay off.
fn view(out_w: u32, out_h: u32) -> ViewParams {
    ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w,
        out_h,
        active: true,
        overlay_layer: -1,
        overlay_color: [0.85, 0.10, 0.10],
        overlay_strength: 0.5,
        crop_preview: false,
    }
}

fn make_texture(
    ctx: &core_pipeline::GpuContext,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
    usage: wgpu::TextureUsages,
) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("texture-target-test"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage,
        view_formats: &[],
    })
}

const TARGET_USAGE: wgpu::TextureUsages =
    wgpu::TextureUsages::RENDER_ATTACHMENT.union(wgpu::TextureUsages::COPY_SRC);

/// A few non-default params: exposure + contrast + a real crop/straighten.
fn params_a() -> DevelopParams {
    DevelopParams {
        exposure: 0.6,
        contrast: 30.0,
        crop: core_pipeline::Crop {
            cx: 0.55,
            cy: 0.45,
            hw: 0.4,
            hh: 0.35,
            angle: 0.04,
            rot90: 0,
        },
        ..Default::default()
    }
}

fn params_b() -> DevelopParams {
    DevelopParams {
        exposure: -0.8,
        contrast: -25.0,
        saturation: 60.0,
        ..Default::default()
    }
}

// 1. The GPU-resident path must produce EXACTLY the bytes the CPU-readback path produces.
#[test]
fn texture_path_matches_render_view_bytes() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe.prepare(&ctx, &ramp(64, 48)).unwrap();
    let params = params_a();
    let v = view(64, 48);

    let via_cpu = pipe.render_view(&ctx, &prep, &params, &v).unwrap();
    let target = make_texture(&ctx, 64, 48, wgpu::TextureFormat::Rgba8Unorm, TARGET_USAGE);
    pipe.render_view_into_texture(&ctx, &prep, &params, &v, None, &target)
        .unwrap();
    let via_gpu = pipe.read_back_texture(&ctx, &target).unwrap();

    assert_eq!(via_cpu.len(), 64 * 48 * 4);
    assert_eq!(via_gpu.len(), via_cpu.len());
    assert_eq!(
        via_gpu, via_cpu,
        "texture-target output must be byte-identical to render_view"
    );
}

// 2. Uniform snapshotting at submit: params A → T1 then IMMEDIATELY (no wait) params B → T2;
//    each texture must hold its own render. wgpu applies `write_buffer` at the next submit, so
//    the un-waited first submission must still develop with A's uniforms.
#[test]
fn back_to_back_submissions_keep_their_own_uniforms() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe.prepare(&ctx, &ramp(64, 48)).unwrap();
    let v = view(64, 48);

    let t1 = make_texture(&ctx, 64, 48, wgpu::TextureFormat::Rgba8Unorm, TARGET_USAGE);
    let t2 = make_texture(&ctx, 64, 48, wgpu::TextureFormat::Rgba8Unorm, TARGET_USAGE);
    let a = params_a();
    let b = params_b();
    // No read_back / poll between the two renders — both submissions are in flight at once.
    pipe.render_view_into_texture(&ctx, &prep, &a, &v, None, &t1)
        .unwrap();
    pipe.render_view_into_texture(&ctx, &prep, &b, &v, None, &t2)
        .unwrap();

    let got1 = pipe.read_back_texture(&ctx, &t1).unwrap();
    let got2 = pipe.read_back_texture(&ctx, &t2).unwrap();
    let want1 = pipe.render_view(&ctx, &prep, &a, &v).unwrap();
    let want2 = pipe.render_view(&ctx, &prep, &b, &v).unwrap();

    assert_eq!(
        got1, want1,
        "T1 must equal render_view(params A) despite T2 being submitted before any wait"
    );
    assert_eq!(got2, want2, "T2 must equal render_view(params B)");
    assert_ne!(
        want1, want2,
        "test is vacuous unless A and B render differently"
    );
}

// 3. Wrong targets: typed Err, no panic. (Size, format, and usage violations.)
#[test]
fn wrong_target_is_rejected() {
    let Some(ctx) = common::gpu() else { return };
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe.prepare(&ctx, &ramp(64, 48)).unwrap();
    let v = view(64, 48);
    let params = DevelopParams::default();

    // Size mismatch (both axes).
    let small = make_texture(&ctx, 32, 48, wgpu::TextureFormat::Rgba8Unorm, TARGET_USAGE);
    assert!(pipe
        .render_view_into_texture(&ctx, &prep, &params, &v, None, &small)
        .is_err());
    let small2 = make_texture(&ctx, 64, 24, wgpu::TextureFormat::Rgba8Unorm, TARGET_USAGE);
    assert!(pipe
        .render_view_into_texture(&ctx, &prep, &params, &v, None, &small2)
        .is_err());

    // Format mismatch.
    let srgb = make_texture(
        &ctx,
        64,
        48,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        TARGET_USAGE,
    );
    assert!(pipe
        .render_view_into_texture(&ctx, &prep, &params, &v, None, &srgb)
        .is_err());

    // Missing RENDER_ATTACHMENT.
    let no_attach = make_texture(
        &ctx,
        64,
        48,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
    );
    assert!(pipe
        .render_view_into_texture(&ctx, &prep, &params, &v, None, &no_attach)
        .is_err());

    // read_back_texture: missing COPY_SRC is an Err, not a validation panic.
    let no_copy_src = make_texture(
        &ctx,
        64,
        48,
        wgpu::TextureFormat::Rgba8Unorm,
        wgpu::TextureUsages::RENDER_ATTACHMENT,
    );
    assert!(pipe.read_back_texture(&ctx, &no_copy_src).is_err());
    let bad_format = make_texture(&ctx, 64, 48, wgpu::TextureFormat::Rgba16Float, TARGET_USAGE);
    assert!(pipe.read_back_texture(&ctx, &bad_format).is_err());

    // The device survived all rejections: a correct render still works.
    let good = make_texture(&ctx, 64, 48, wgpu::TextureFormat::Rgba8Unorm, TARGET_USAGE);
    pipe.render_view_into_texture(&ctx, &prep, &params, &v, None, &good)
        .unwrap();
    assert_eq!(
        pipe.read_back_texture(&ctx, &good).unwrap().len(),
        64 * 48 * 4
    );
}
