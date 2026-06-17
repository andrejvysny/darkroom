//! Detection eval harness — drives the REAL `core_analyze::ObjectDetector` through the production
//! decode path (`core_raw::preview_image` → downscale ≤1024 → detect), so it reflects the live
//! precision-gated pipeline (unlike `detect_one`, which carries its own spike copy of the decode).
//!
//! Usage:
//!   cargo run -p core-analyze --example detect_eval                       # default = the 4 known FP frames
//!   cargo run -p core-analyze --example detect_eval -- img1.CR3 img2.jpg  # explicit files
//!   MODEL=/path/to/dfine_m.onnx cargo run -p core-analyze --example detect_eval
//!
//! Prints per-file detections (label / category / confidence / bbox). With no positive subjects in
//! the inputs, the PASS criterion is **zero People/Animals** detections.

use std::path::{Path, PathBuf};

use std::sync::Arc;

use anyhow::{Context, Result};
use core_analyze::{ObjectDetector, Verifier};
use image::imageops::FilterType;

const ANALYZE_EDGE: u32 = 1024; // mirror src-tauri/src/analysis.rs

/// Known false-positive frames (no people, no animals) used as the precision regression set.
const DEFAULT_FRAMES: &[&str] = &[
    "library/nature/_55A4115.CR3",
    "library/nature/_55A4063.CR3",
    "library/nature/_55A4048.CR3",
    "library/nature/_55A4049.CR3",
];

fn model_path() -> PathBuf {
    if let Ok(p) = std::env::var("MODEL") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home)
        .join("Library/Application Support/com.andrejvysny.darkroom/models/dfine_m.onnx")
}

fn is_raw(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("cr3" | "cr2" | "arw" | "nef" | "dng")
    )
}

/// Production decode: RAW embedded preview (or a plain image file) → sRGB → downscale to ANALYZE_EDGE.
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
    let mp = model_path();
    println!("model: {}", mp.display());
    let mut detector = ObjectDetector::new(&mp, "eval").context("load detector")?;

    // Attach the CLIP verifier if its files are present (same models dir). Skipped via NO_VERIFY=1.
    let mdir = mp.parent().unwrap_or(std::path::Path::new("."));
    let (v_vis, v_txt, v_tok) = (
        mdir.join("mobileclip/vision_model.onnx"),
        mdir.join("mobileclip/text_model.onnx"),
        mdir.join("mobileclip/tokenizer.json"),
    );
    if std::env::var("NO_VERIFY").is_err() && v_vis.exists() && v_txt.exists() && v_tok.exists() {
        println!("verifier: MobileCLIP-S1 attached");
        let v = Verifier::new(&v_vis, &v_txt, &v_tok).context("load verifier")?;
        detector = detector.with_verifier(Arc::new(v));
    } else {
        println!("verifier: none (D-FINE gates only)");
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let use_default = args.is_empty();
    let frames: Vec<String> = if use_default {
        DEFAULT_FRAMES.iter().map(|s| s.to_string()).collect()
    } else {
        args
    };

    let (mut people, mut animals, mut total) = (0usize, 0usize, 0usize);
    for f in &frames {
        let path = Path::new(f);
        if !path.exists() {
            println!("\n{f}\n  (missing — skipped)");
            continue;
        }
        let img = match decode_srgb(path) {
            Ok(i) => i,
            Err(e) => {
                println!("\n{f}\n  decode error: {e:#}");
                continue;
            }
        };
        let dets = detector.detect(&img).context("detect")?;
        println!("\n{f}  ({}×{})", img.width(), img.height());
        if dets.is_empty() {
            println!("  (no detections) ✓");
        }
        for d in &dets {
            total += 1;
            match d.category.as_str() {
                "People" => people += 1,
                "Animals" => animals += 1,
                _ => {}
            }
            println!(
                "  {:<8} {:<8} {:.3}  bbox=[{:.3} {:.3} {:.3} {:.3}]",
                d.category, d.label, d.confidence, d.bbox[0], d.bbox[1], d.bbox[2], d.bbox[3]
            );
        }
    }

    println!("\n── summary ──");
    println!("detections: {total} (People {people}, Animals {animals})");
    if use_default {
        // Default set is the all-negative FP regression set.
        if people == 0 && animals == 0 {
            println!("PASS: 0 People/Animals on the known false-positive frames ✓");
        } else {
            println!("FAIL: expected 0 People/Animals on the false-positive frames ✗");
            std::process::exit(1);
        }
    }
    Ok(())
}
