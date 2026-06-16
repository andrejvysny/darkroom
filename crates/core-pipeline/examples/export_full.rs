//! Phase-5 export gate: full-resolution develop render → PNG + JPEG. Run:
//!   cargo run -p core-pipeline --example export_full [FILE.CR3]

use core_pipeline::{rgba8_to_jpeg, rgba8_to_png, DevelopParams, DevelopPipeline, GpuContext};
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
        .expect("no CR3");
    let src = source_from_path(&path)?;
    let lin = develop_linear(&src)?;
    println!("full-res linear: {}x{}", lin.width, lin.height);

    let ctx = GpuContext::new()?;
    let pipe = DevelopPipeline::new(&ctx);
    let params = DevelopParams {
        exposure: 0.3,
        contrast: 15.0,
        saturation: 12.0,
        ..Default::default()
    };

    let t = Instant::now();
    let rgba = pipe.render_once(&ctx, &lin, &params)?;
    println!("full-res GPU render in {:.1?}", t.elapsed());

    let png = rgba8_to_png(&rgba, lin.width, lin.height)?;
    std::fs::write("/tmp/darkroom-export.png", &png)?;
    let jpg = rgba8_to_jpeg(&rgba, lin.width, lin.height, 92)?;
    std::fs::write("/tmp/darkroom-export.jpg", &jpg)?;
    println!(
        "wrote PNG ({} KB) + JPEG ({} KB) at {}x{}",
        png.len() / 1024,
        jpg.len() / 1024,
        lin.width,
        lin.height
    );
    Ok(())
}
