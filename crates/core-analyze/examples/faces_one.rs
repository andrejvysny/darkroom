//! Phase 0 face-pipeline validation: decode image(s) → SCRFD detect → align → ArcFace embed.
//!
//! Usage: cargo run -p core-analyze --example faces_one -- IMAGE [IMAGE ...] [--edge N]
//! Models (SCRFD + ArcFace, ~190 MB) are downloaded once into the models dir (override with
//! `DARKROOM_MODELS_DIR`; default = the app's Application Support models dir).
//!
//! Prints per-face box, score, embedding L2-norm (≈1.0) and writes aligned 112×112 crops to
//! `/tmp/darkroom-faces/`. With ≥2 detected faces it also prints a pairwise cosine-distance matrix —
//! a quick check that embeddings discriminate identity (same person ≪ different) and a way to
//! calibrate the clustering threshold.

use anyhow::{Context, Result};
use core_analyze::models::{ModelStore, FACE_DETECTOR_FILES, FACE_EMBEDDER_FILES};
use core_analyze::FaceAnalyzer;
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

fn default_image() -> Option<PathBuf> {
    let root = Path::new("library");
    if !root.exists() {
        return None;
    }
    let mut stack = vec![root.to_path_buf()];
    let mut found = None;
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if is_raw(&p) {
                    found = Some(p);
                }
            }
        }
    }
    found
}

fn models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("DARKROOM_MODELS_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/com.andrejvysny.darkroom/models")
}

fn main() -> Result<()> {
    let mut images: Vec<PathBuf> = Vec::new();
    let mut det_edge: u32 = 640;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--edge" {
            det_edge = args.next().and_then(|s| s.parse().ok()).unwrap_or(640);
        } else {
            images.push(PathBuf::from(a));
        }
    }
    if images.is_empty() {
        images.extend(default_image());
    }
    if images.is_empty() {
        anyhow::bail!("no images given and no RAW found under library/");
    }

    let store = ModelStore::new(models_dir());
    println!("ensuring face models in {:?} …", models_dir());
    store
        .ensure(FACE_DETECTOR_FILES, |i, n| println!("  detector {i}/{n}"))
        .context("download SCRFD")?;
    store
        .ensure(FACE_EMBEDDER_FILES, |i, n| println!("  embedder {i}/{n}"))
        .context("download ArcFace")?;

    let analyzer = FaceAnalyzer::new(
        &store.face_detector_path(),
        &store.face_embedder_path(),
        det_edge,
    )
    .context("build FaceAnalyzer")?;

    let out_dir = Path::new("/tmp/darkroom-faces");
    std::fs::create_dir_all(out_dir).ok();
    let mut all: Vec<(String, Vec<f32>)> = Vec::new();

    for img_path in &images {
        let rgb = load_srgb(img_path)?;
        let stem = img_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("img")
            .to_string();
        let t0 = Instant::now();
        let faces = analyzer.detect_embed(&rgb)?;
        println!(
            "\n{img_path:?}  ({}x{}, edge={det_edge}): {} face(s) in {:.0} ms",
            rgb.width(),
            rgb.height(),
            faces.len(),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        for (i, f) in faces.iter().enumerate() {
            let norm = f.embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
            println!(
                "  face {i}: score {:.2}  bbox [{:.3},{:.3},{:.3},{:.3}]  L2 {:.4}  quality {:.0}",
                f.det_score, f.bbox[0], f.bbox[1], f.bbox[2], f.bbox[3], norm, f.quality
            );
            let label = format!("{stem}#{i}");
            let aligned = core_analyze::face_aligner::align(&rgb, &f.kps);
            let _ = aligned.save(out_dir.join(format!("{label}.png")));
            all.push((label, f.embedding.clone()));
        }
    }
    println!("\naligned crops → {out_dir:?}");

    if all.len() >= 2 {
        println!("\npairwise cosine distance (1 − cos; same person ≪ different):");
        print!("{:>12}", "");
        for (l, _) in &all {
            print!("{l:>10}");
        }
        println!();
        for (la, ea) in &all {
            print!("{la:>12}");
            for (_, eb) in &all {
                let dot: f32 = ea.iter().zip(eb).map(|(x, y)| x * y).sum();
                print!("{:>10.3}", 1.0 - dot);
            }
            println!();
        }
    }
    Ok(())
}
