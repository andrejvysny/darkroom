//! No-GUI panorama harness: a directory of raws → LinearRaw DNG (+ sRGB JPEG proof).
//!
//! ```bash
//! cargo run --release -p core-raw --example stitch_cr3 -- <dir-of-raws> [out.dng]
//! ```
//!
//! The full app path (`src-tauri/src/panorama.rs`) does exactly this plus catalog registration;
//! this example is the fast feedback loop for stitch quality on real captures.

use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: stitch_cr3 <dir-of-raws> [out.dng]"));
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("pano_out.dng"));

    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| {
                    ["cr3", "cr2", "arw", "nef", "dng"].contains(&s.to_ascii_lowercase().as_str())
                })
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    anyhow::ensure!(paths.len() >= 2, "need at least 2 raws in {}", dir.display());
    println!("{} source frames", paths.len());

    let t = Instant::now();
    let mut frames = Vec::new();
    let mut metas = Vec::new();
    for p in &paths {
        let src = core_raw::source_from_path(p)?;
        let native = core_raw::develop_camera_native(&src)?;
        println!(
            "  {}: {}x{}",
            p.file_name().unwrap().to_string_lossy(),
            native.width,
            native.height
        );
        frames.push(core_pano::Frame {
            width: native.width as usize,
            height: native.height as usize,
            rgb: native.data,
            focal_seed_px: None,
        });
        metas.push(native.meta);
    }
    println!("decode: {:.1}s", t.elapsed().as_secs_f32());

    let t = Instant::now();
    let opt = core_pano::StitchOptions {
        projection: core_pano::Projection::Auto,
        boundary_warp: 0.0,
        auto_crop: true,
        max_long_side: 12_000,
        preview: false,
    };
    let result = core_pano::stitch(&frames, &opt, &|phase| println!("  phase: {phase:?}"))?;
    drop(frames);
    println!(
        "stitch: {:.1}s → {}x{} ({:?}, capped={})",
        t.elapsed().as_secs_f32(),
        result.width,
        result.height,
        result.projection_used,
        result.capped
    );

    let t = Instant::now();
    let meta = &metas[result.reference_index];
    core_raw::write_pano_dng(
        &out,
        result.width as u32,
        result.height as u32,
        &result.rgb,
        meta,
    )?;
    println!("DNG: {:.1}s → {}", t.elapsed().as_secs_f32(), out.display());

    // sRGB proof image next to the DNG for quick eyeballing.
    let jpeg = core_raw::native_to_srgb_jpeg(
        result.width as u32,
        result.height as u32,
        &result.rgb,
        meta,
        4096,
        90,
    )?;
    let jpg_path = out.with_extension("jpg");
    std::fs::write(&jpg_path, jpeg)?;
    println!("proof JPEG → {}", jpg_path.display());
    Ok(())
}
