//! Phase-2 GPU gate: decode a CR3 → linear → wgpu develop render → PNG. Run:
//!   cargo run -p core-pipeline --example render_one [FILE.CR3]

use core_pipeline::{rgba8_to_png, DevelopParams, DevelopPipeline, GpuContext};
use core_raw::{develop_linear, source_from_path};
use std::path::PathBuf;
use std::time::Instant;

fn first_cr3() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026/2026-06-06")
        .canonicalize()
        .ok()?;
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v.into_iter().next()
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .or_else(first_cr3)
        .expect("no CR3 file");
    println!("file: {}", path.display());

    let t = Instant::now();
    let src = source_from_path(&path)?;
    let lin = develop_linear(&src)?;
    println!(
        "develop_linear: {}x{} in {:.1?}",
        lin.width,
        lin.height,
        t.elapsed()
    );

    let preview = lin.downscaled(1400);
    let ctx = GpuContext::new()?;
    println!("GPU adapter OK");
    let pipe = DevelopPipeline::new(&ctx);

    let t = Instant::now();
    let prepared = pipe.prepare(&ctx, &preview)?;
    println!("prepare (upload) in {:.1?}", t.elapsed());

    let t = Instant::now();
    let rgba = pipe.render(&ctx, &prepared, &DevelopParams::default())?;
    println!(
        "render default {}x{} in {:.1?}",
        preview.width,
        preview.height,
        t.elapsed()
    );
    std::fs::write(
        "/tmp/darkroom-dev-default.png",
        rgba8_to_png(&rgba, preview.width, preview.height)?,
    )?;

    let edited = DevelopParams {
        exposure: 0.8,
        contrast: 30.0,
        saturation: 25.0,
        temp: 18.0,
        shadows: 30.0,
        ..Default::default()
    };
    let t = Instant::now();
    let rgba2 = pipe.render(&ctx, &prepared, &edited)?;
    println!("render edited (reused prepare) in {:.1?}", t.elapsed());
    std::fs::write(
        "/tmp/darkroom-dev-edited.png",
        rgba8_to_png(&rgba2, preview.width, preview.height)?,
    )?;

    println!("wrote /tmp/darkroom-dev-default.png and /tmp/darkroom-dev-edited.png");
    Ok(())
}
