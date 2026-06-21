//! Visual check for crop + straighten: decode a CR3 → GPU develop with geometry → PNGs. Run:
//!   cargo run -p core-pipeline --example crop_demo [FILE.CR3]
//! Writes: straighten-only (autozoom), 16:9 crop preview (letterboxed), 16:9 true-dims export.

use core_pipeline::{crop_rgba8, rgba8_to_png, Crop, DevelopParams, DevelopPipeline, GpuContext};
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
    let src = source_from_path(&path)?;
    let lin = develop_linear(&src)?;
    let preview = lin.downscaled(1400);
    let (w, h) = (preview.width, preview.height);
    let ctx = GpuContext::new()?;
    let pipe = DevelopPipeline::new(&ctx);
    let prepared = pipe.prepare(&ctx, &preview)?;

    // 1. Straighten only (full frame) — autozoom should hide the rotated corners.
    let straighten = DevelopParams {
        crop: Crop {
            angle: 6.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let rgba = pipe.render(&ctx, &prepared, &straighten)?;
    std::fs::write("/tmp/darkroom-straighten.png", rgba8_to_png(&rgba, w, h)?)?;

    // 2. 16:9 crop + small straighten. Preview render (letterbox-fit into the preview frame).
    let aspect = w as f32 / h as f32;
    let k = (16.0 / 9.0) / aspect; // hw/hh
    let crop = Crop {
        cx: 0.5,
        cy: 0.5,
        hw: if k >= 1.0 { 0.5 } else { 0.5 * k },
        hh: if k >= 1.0 { 0.5 / k } else { 0.5 },
        angle: 3.0,
    };
    let params = DevelopParams {
        crop,
        ..Default::default()
    };
    let rgba = pipe.render(&ctx, &prepared, &params)?;
    std::fs::write("/tmp/darkroom-crop-preview.png", rgba8_to_png(&rgba, w, h)?)?;

    // 3. True-dims export: crop the letterbox content rect (plain pixel copy).
    let (cx, cy, cw, ch) = crop.export_rect(w, h);
    let cropped = crop_rgba8(&rgba, w, cx, cy, cw, ch);
    std::fs::write(
        "/tmp/darkroom-crop-export.png",
        rgba8_to_png(&cropped, cw, ch)?,
    )?;
    println!(
        "export rect {cw}x{ch} (aspect {:.3}) from {w}x{h}",
        cw as f32 / ch as f32
    );
    println!("wrote /tmp/darkroom-straighten.png, -crop-preview.png, -crop-export.png");
    Ok(())
}
