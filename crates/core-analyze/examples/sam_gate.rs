//! Headless gate for interactive segmentation (AI masking): ensure MobileSAM is downloaded, encode an
//! image once, decode a point prompt, and assert a non-trivial mask. Writes the mask PNG to the temp
//! dir for eyeballing. Fast feedback without launching the app.
//!
//! Usage: cargo run -p core-analyze --example sam_gate -- [image] [x_norm] [y_norm] [tier]
//!   image  — RAW (embedded preview) or jpg/png; default = committed CR3 fixture
//!   x/y    — normalized [0,1] click, default center (0.5, 0.5)
//!   tier   — realtime | balanced | max (default realtime)

use anyhow::{Context, Result};
use core_analyze::models::{ModelStore, SamTier};
use core_analyze::sam::{Prompt, Segmenter};
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
            .context("decode embedded preview")?
            .to_rgb8())
    } else {
        Ok(image::open(path)?.to_rgb8())
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let image = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("library/2026/2026-06-06/_55A3947.CR3"));
    let px: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let py: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0.5);
    let tier = SamTier::from_tag(args.next().as_deref().unwrap_or("realtime"));

    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cache/darkroom-models");
    let store = ModelStore::new(dir.clone());
    println!("ensuring {} SAM models in {dir:?} ...", tier.as_str());
    store.ensure(tier.files(), |d, t| println!("  fetched {d}/{t}"))?;

    let seg = Segmenter::new(
        &store.sam_encoder_path(tier),
        &store.sam_decoder_path(tier),
        tier.preproc(),
        tier.encoder_cpu(),
    )?;
    let img = load_srgb(&image)?;
    println!(
        "image {}: {}x{}",
        image.display(),
        img.width(),
        img.height()
    );

    let t0 = Instant::now();
    let emb = seg.encode(&img)?;
    println!(
        "encode {:.0} ms -> work {}x{}, embedding {} floats",
        t0.elapsed().as_secs_f64() * 1000.0,
        emb.work_w,
        emb.work_h,
        emb.data.len()
    );

    let t1 = Instant::now();
    let mask = seg.decode(
        &emb,
        &[Prompt {
            x: px,
            y: py,
            positive: true,
        }],
        None,
        None,
    )?;
    let dt = t1.elapsed().as_secs_f64() * 1000.0;
    let pos = mask.alpha.iter().filter(|&&v| v > 0).count();
    let frac = pos as f64 / (mask.width as usize * mask.height as usize) as f64;
    println!(
        "decode {dt:.1} ms -> mask {}x{}, iou {:.3}, coverage {:.2}% (click {px},{py})",
        mask.width,
        mask.height,
        mask.iou,
        frac * 100.0
    );

    let mut g = image::GrayImage::new(mask.width, mask.height);
    for (i, p) in g.pixels_mut().enumerate() {
        *p = image::Luma([mask.alpha[i]]);
    }
    let out = std::env::temp_dir().join("sam_gate_mask.png");
    g.save(&out)?;
    println!("wrote {out:?}");

    anyhow::ensure!(
        frac > 0.0005 && frac < 0.98,
        "implausible mask coverage {:.4} — pipeline likely wrong",
        frac
    );
    println!("OK");
    Ok(())
}
