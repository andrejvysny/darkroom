//! Phase 1 validation: Florence-2 captioner end-to-end.
//! Usage: cargo run -p core-analyze --example caption_one -- [florence2_dir] [image]
//! Defaults: dir = ~/.cache/darkroom-models/florence2, image = ~/.cache/darkroom-models/cats.jpg.

use anyhow::{Context, Result};
use core_analyze::Captioner;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn is_raw(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("cr3" | "cr2" | "arw" | "nef" | "dng")
    )
}

fn load_srgb(path: &Path) -> Result<image::RgbImage> {
    if is_raw(path) {
        let src = core_raw::source_from_path(path).with_context(|| format!("open {path:?}"))?;
        Ok(core_raw::preview_image(&src)
            .context("decode preview")?
            .to_rgb8())
    } else {
        Ok(image::open(path)?.to_rgb8())
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let cache =
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/darkroom-models");
    let dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("florence2"));
    let image = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("cats.jpg"));
    println!("dir: {dir:?}\nimage: {image:?}");

    let t_load = Instant::now();
    let cap = Captioner::new(&dir, &dir.join("tokenizer.json"), "florence2-base-ft-q4f16")?;
    println!("loaded Florence-2 sessions in {:?}", t_load.elapsed());

    let img = load_srgb(&image)?;
    let t0 = Instant::now();
    let caption = cap.caption(&img)?;
    println!("\ncaption ({:?}): {caption}", t0.elapsed());
    Ok(())
}
