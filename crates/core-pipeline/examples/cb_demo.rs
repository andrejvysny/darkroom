//! Visual + smoke check for Color-balance-RGB (@binding(14)): render one CR3 at the default look and
//! with a teal-shadows / warm-highlights split-tone grade. Run:
//!   cargo run -p core-pipeline --example cb_demo [FILE.CR3]
//! → /tmp/darkroom-cb-default.png and /tmp/darkroom-cb-graded.png

use core_pipeline::{rgba8_to_png, CbRgb, DevelopParams, DevelopPipeline, GpuContext};
use core_raw::{develop_linear, source_from_path};
use std::path::PathBuf;

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

    let lin = develop_linear(&source_from_path(&path)?)?.downscaled(1400);
    let ctx = GpuContext::new()?;
    let pipe = DevelopPipeline::new(&ctx);
    let prep = pipe.prepare(&ctx, &lin)?;

    let def = pipe.render(&ctx, &prep, &DevelopParams::default())?;
    std::fs::write(
        "/tmp/darkroom-cb-default.png",
        rgba8_to_png(&def, lin.width, lin.height)?,
    )?;

    // Split-tone: cool shadows (toward blue), warm highlights (toward red/green), gentle pop.
    let graded = DevelopParams {
        cb_rgb: CbRgb {
            shadows: [-0.12, -0.02, 0.18],
            highlights: [0.16, 0.05, -0.10],
            contrast: 0.12,
            saturation: 0.10,
            ..CbRgb::default()
        },
        ..DevelopParams::default()
    };
    let out = pipe.render(&ctx, &prep, &graded)?;
    std::fs::write(
        "/tmp/darkroom-cb-graded.png",
        rgba8_to_png(&out, lin.width, lin.height)?,
    )?;

    println!("wrote /tmp/darkroom-cb-default.png and /tmp/darkroom-cb-graded.png");
    Ok(())
}
