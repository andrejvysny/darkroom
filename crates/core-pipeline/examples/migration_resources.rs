//! Bounded synthetic large-image resource baseline. Run one case per process:
//! `cargo run --release -p core-pipeline --example migration_resources -- 32mp [--masked]`
//! Cases: 32mp, 60mp, pano. RSS is process memory, not GPU-resident memory.

use anyhow::{bail, Context, Result};
use core_pipeline::{
    ComponentKind, DevelopParams, DevelopPipeline, GpuContext, LocalAdjust, Mask, MaskComponent,
    MaskOp, ViewParams, MASK_CAP,
};
use core_raw::LinearImage;
use std::{process::Command, time::Instant};

struct Case {
    name: &'static str,
    width: u32,
    height: u32,
    masked: bool,
}

fn arguments() -> Result<Case> {
    let mut args = std::env::args().skip(1);
    let name = args
        .next()
        .context("usage: migration_resources <32mp|60mp|pano> [--masked]")?;
    let (name, width, height) = match name.as_str() {
        "32mp" => ("32mp", 6960, 4640),
        "60mp" => ("60mp", 10000, 6000),
        "pano" => ("pano", 12000, 4000),
        _ => bail!("unknown case: {name}"),
    };
    let masked = match args.next().as_deref() {
        None => false,
        Some("--masked") => true,
        Some(arg) => bail!("unknown argument: {arg}"),
    };
    if args.next().is_some() {
        bail!("too many arguments");
    }
    Ok(Case {
        name,
        width,
        height,
        masked,
    })
}

fn rss_kib() -> Result<u64> {
    let output = Command::new("ps")
        .args(["-p", &std::process::id().to_string(), "-o", "rss="])
        .output()
        .context("run ps for RSS")?;
    if !output.status.success() {
        bail!("ps failed with status {}", output.status);
    }
    let text = std::str::from_utf8(&output.stdout).context("ps output is not UTF-8")?;
    text.trim().parse().context("parse ps RSS KiB")
}

fn synthetic(case: &Case, pixels: usize) -> Result<LinearImage> {
    let samples = pixels.checked_mul(3).context("sample count overflow")?;
    let mut data = Vec::new();
    data.try_reserve_exact(samples)
        .context("allocate synthetic input")?;
    for y in 0..case.height {
        for x in 0..case.width {
            let r = x as f32 / case.width as f32;
            let g = y as f32 / case.height as f32;
            data.extend_from_slice(&[r, g, 0.25]);
        }
    }
    Ok(LinearImage {
        width: case.width,
        height: case.height,
        data,
    })
}

fn parameters(masked: bool) -> DevelopParams {
    let mut params = DevelopParams::default();
    if masked {
        params.masks.push(Mask {
            name: "synthetic-linear".into(),
            components: vec![MaskComponent {
                kind: ComponentKind::Linear {
                    p0: [0.2, 0.5],
                    p1: [0.8, 0.5],
                },
                op: MaskOp::Add,
                invert: false,
                feather: false,
            }],
            adjust: LocalAdjust {
                exposure: 0.5,
                ..Default::default()
            },
            opacity: 1.0,
            enabled: true,
        });
    }
    params
}

fn view(case: &Case) -> ViewParams {
    let longest = u64::from(case.width.max(case.height));
    let scaled = |v: u32| ((u64::from(v) * 1400 + longest / 2) / longest).max(1) as u32;
    ViewParams {
        origin: [0.0, 0.0],
        size: [1.0, 1.0],
        out_w: scaled(case.width),
        out_h: scaled(case.height),
        active: true,
        overlay_layer: -1,
        overlay_color: [0.85, 0.10, 0.10],
        overlay_strength: 0.5,
        crop_preview: false,
    }
}

fn run(case: &Case, pixels: usize) -> Result<()> {
    let image = synthetic(case, pixels)?;
    println!("rss_after_input_kib={}", rss_kib()?);
    let ctx = GpuContext::new().context("create GPU context")?;
    println!(
        "adapter={:?}\nbackend={:?}",
        ctx.info.name, ctx.info.backend
    );
    let pipe = DevelopPipeline::new(&ctx);
    let started = Instant::now();
    let prepared = pipe.prepare(&ctx, &image).context("prepare image")?;
    println!("prepare_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
    println!("rss_after_prepare_kib={}", rss_kib()?);
    drop(image);
    let view = view(case);
    println!(
        "preview_width={}\npreview_height={}",
        view.out_w, view.out_h
    );
    let started = Instant::now();
    let output = pipe
        .render_view(&ctx, &prepared, &parameters(case.masked), &view)
        .context("render preview")?;
    println!("render_ms={:.3}", started.elapsed().as_secs_f64() * 1000.0);
    println!("rss_after_render_kib={}", rss_kib()?);
    drop(output);
    drop(prepared);
    drop(pipe);
    drop(ctx);
    Ok(())
}

fn main() -> Result<()> {
    let case = arguments()?;
    let pixels = (case.width as usize)
        .checked_mul(case.height as usize)
        .context("pixel count overflow")?;
    let input_bytes = pixels
        .checked_mul(3)
        .and_then(|n| n.checked_mul(std::mem::size_of::<f32>()))
        .context("input byte count overflow")?;
    let mask_bytes = pixels
        .checked_mul(MASK_CAP)
        .and_then(|n| n.checked_mul(2))
        .context("mask byte count overflow")?;
    println!(
        "case={}\nsource=synthetic_linear_gradient\nwidth={}\nheight={}\nmasked={}",
        case.name, case.width, case.height, case.masked
    );
    println!("cpu_input_nominal_bytes={input_bytes}\nmask_array_nominal_bytes={mask_bytes}");
    println!("rss_before_kib={}", rss_kib()?);
    run(&case, pixels)?;
    println!("rss_after_drop_kib={}", rss_kib()?);
    Ok(())
}
