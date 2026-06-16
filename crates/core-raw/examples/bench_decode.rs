//! Compares full-resolution vs half-resolution (superpixel) linear decode timing.
//! Run (release for realistic numbers):
//!   cargo run -p core-raw --release --example bench_decode [FILE_OR_DIR]

use std::path::PathBuf;
use std::time::Instant;

fn first_raw(arg: Option<String>) -> Option<PathBuf> {
    let p = PathBuf::from(arg.unwrap_or_else(|| {
        format!(
            "{}/../../library/2026/2026-06-06",
            env!("CARGO_MANIFEST_DIR")
        )
    }));
    if p.is_file() {
        return Some(p);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(p)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| {
                    matches!(
                        s.to_ascii_lowercase().as_str(),
                        "cr3" | "arw" | "cr2" | "nef" | "dng"
                    )
                })
                .unwrap_or(false)
        })
        .collect();
    files.sort();
    files.into_iter().next()
}

fn main() -> anyhow::Result<()> {
    let Some(path) = first_raw(std::env::args().nth(1)) else {
        eprintln!("no RAW file found");
        return Ok(());
    };
    println!("file: {}", path.display());

    let src = core_raw::source_from_path(&path)?;
    let t = Instant::now();
    let full = core_raw::develop_linear(&src)?;
    let full_ms = t.elapsed();
    println!(
        "develop_linear (full):       {:>5} x {:<5}  {:?}",
        full.width, full.height, full_ms
    );

    let src2 = core_raw::source_from_path(&path)?;
    let t = Instant::now();
    let prev = core_raw::develop_linear_preview(&src2)?;
    let prev_ms = t.elapsed();
    println!(
        "develop_linear_preview (½):  {:>5} x {:<5}  {:?}",
        prev.width, prev.height, prev_ms
    );

    let t = Instant::now();
    let scaled = prev.downscale_into(1600);
    let scale_ms = t.elapsed();
    println!(
        "  + downscale_into(1600):    {:>5} x {:<5}  {:?}",
        scaled.width, scaled.height, scale_ms
    );

    let speedup = full_ms.as_secs_f64() / prev_ms.as_secs_f64().max(1e-9);
    println!(
        "decode speedup: {:.2}x   (preview decode+downscale total: {:?})",
        speedup,
        prev_ms + scale_ms
    );
    Ok(())
}
