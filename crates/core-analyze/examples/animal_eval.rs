//! MegaDetector animal-detection eval — drives the real `MegaDetector` (+ optional CLIP verifier)
//! through the production decode path.
//!
//! Usage:
//!   cargo run -p core-analyze --example animal_eval -- [files...]
//!   MD_SIZE=640 NO_VERIFY=1 cargo run -p core-analyze --example animal_eval -- img.jpg
//! Default files = the 4 known false-positive frames (expect 0 animals) + nothing else.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use core_analyze::{MegaDetector, Verifier};
use image::imageops::FilterType;

const ANALYZE_EDGE: u32 = 1024;

const DEFAULT_FRAMES: &[&str] = &[
    "library/nature/_55A4115.CR3",
    "library/nature/_55A4063.CR3",
    "library/nature/_55A4048.CR3",
    "library/nature/_55A4049.CR3",
];

fn models_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.andrejvysny.darkroom/models")
}

fn is_raw(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("cr3" | "cr2" | "arw" | "nef" | "dng")
    )
}

fn decode_srgb(path: &Path) -> Result<image::RgbImage> {
    let img = if is_raw(path) {
        let src =
            core_raw::source_from_path(path).with_context(|| format!("open {}", path.display()))?;
        core_raw::preview_image(&src)
            .with_context(|| format!("preview {}", path.display()))?
            .to_rgb8()
    } else {
        image::open(path)
            .with_context(|| format!("open {}", path.display()))?
            .to_rgb8()
    };
    let (w, h) = (img.width(), img.height());
    let m = w.max(h);
    if m > ANALYZE_EDGE {
        let s = ANALYZE_EDGE as f32 / m as f32;
        Ok(image::imageops::resize(
            &img,
            (w as f32 * s) as u32,
            (h as f32 * s) as u32,
            FilterType::Triangle,
        ))
    } else {
        Ok(img)
    }
}

fn main() -> Result<()> {
    let mdir = models_dir();
    let size: u32 = std::env::var("MD_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1280);
    let mp = mdir.join("megadetector/md_v5a_dynamic.onnx");
    println!("model: {} @ {size}²", mp.display());
    let mut md = MegaDetector::new(&mp, "eval", size).context("load megadetector")?;

    let (v_vis, v_txt, v_tok) = (
        mdir.join("mobileclip/vision_model.onnx"),
        mdir.join("mobileclip/text_model.onnx"),
        mdir.join("mobileclip/tokenizer.json"),
    );
    if std::env::var("NO_VERIFY").is_err() && v_vis.exists() && v_txt.exists() && v_tok.exists() {
        println!("verifier: MobileCLIP-S1 attached");
        md = md.with_verifier(Arc::new(Verifier::new(&v_vis, &v_txt, &v_tok)?));
    } else {
        println!("verifier: none");
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let frames: Vec<String> = if args.is_empty() {
        DEFAULT_FRAMES.iter().map(|s| s.to_string()).collect()
    } else {
        args
    };

    let mut total_animals = 0usize;
    for f in &frames {
        let path = Path::new(f);
        if !path.exists() {
            println!("\n{f}\n  (missing — skipped)");
            continue;
        }
        let img = decode_srgb(path)?;
        let dets = md.detect(&img).context("detect")?;
        println!("\n{f}  ({}×{})", img.width(), img.height());
        if dets.is_empty() {
            println!("  (no animals)");
        }
        for d in &dets {
            total_animals += 1;
            println!(
                "  {:<8} {:.3}  bbox=[{:.3} {:.3} {:.3} {:.3}]",
                d.label, d.confidence, d.bbox[0], d.bbox[1], d.bbox[2], d.bbox[3]
            );
        }
    }
    println!("\n── summary ──\nanimal detections: {total_animals}");
    Ok(())
}
