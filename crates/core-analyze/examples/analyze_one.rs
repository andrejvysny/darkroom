//! Phase 1 integration: full Analyzer pipeline (detector → captioner) via the registry, with the
//! caption stage reading detection labels from `prior`.
//! Usage: cargo run -p core-analyze --example analyze_one -- <detector.onnx> <florence2_dir> <image>
//! Defaults: ~/.cache/darkroom-models/{dfine_s.onnx, florence2_staged, cats.jpg}.

use anyhow::{Context, Result};
use core_analyze::{
    AnalysisCtx, AnalysisRecord, AnalyzerRegistry, CaptionPayload, Captioner, DetectionPayload,
    ObjectDetector,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    let detector_path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("dfine_s.onnx"));
    let florence_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("florence2_staged"));
    let image = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("cats.jpg"));

    let mut registry = AnalyzerRegistry::new();
    registry.register(Arc::new(ObjectDetector::new(&detector_path, "dfine-test")?));
    registry.register(Arc::new(Captioner::new(
        &florence_dir,
        &florence_dir.join("tokenizer.json"),
        "florence-test",
    )?));

    let img = load_srgb(&image)?;
    println!("image: {image:?} ({}x{})", img.width(), img.height());

    // Run analyzers in order; each sees prior records (caption folds in detection labels).
    let t0 = Instant::now();
    let mut records: Vec<AnalysisRecord> = Vec::new();
    for a in registry.analyzers() {
        let ctx = AnalysisCtx {
            image_id: 1,
            content_hash_hex: "test",
            image: &img,
            prior: &records,
        };
        records.push(a.analyze(&ctx)?);
    }
    println!("analyzed in {:?}\n", t0.elapsed());

    for rec in &records {
        match rec.analyzer_id.as_str() {
            "object_detection" => {
                let p: DetectionPayload = rec.parse().unwrap_or_default();
                println!("detections ({}):", p.detections.len());
                for d in &p.detections {
                    println!(
                        "  {:<10} {:>5.1}%  {}",
                        d.label,
                        d.confidence * 100.0,
                        d.category
                    );
                }
            }
            "caption" => {
                let p: CaptionPayload = rec.parse().unwrap_or_default();
                println!("\ncaption: {}", p.caption);
                println!("keywords: {:?}", p.keywords);
            }
            _ => {}
        }
    }
    Ok(())
}
