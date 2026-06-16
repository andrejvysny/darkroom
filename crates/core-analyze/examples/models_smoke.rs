//! Phase 3 validation: ModelStore download (ureq) + the production D-FINE-M detector end-to-end.
//! Downloads the detector into a temp models dir, then detects on a test image.
//! Usage: cargo run -p core-analyze --example models_smoke -- [image]

use anyhow::Result;
use core_analyze::models::{ModelStore, DETECTOR_FILES};
use core_analyze::ObjectDetector;
use std::path::PathBuf;

fn main() -> Result<()> {
    let cache =
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/darkroom-models");
    let image = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| cache.join("cats.jpg"));

    // Reuse the persistent cache as the "models dir" so reruns don't re-download.
    let store = ModelStore::new(cache.clone());
    println!("ensuring detector model ({} file)...", DETECTOR_FILES.len());
    store.ensure(DETECTOR_FILES, |done, total| {
        println!("  models {done}/{total}")
    })?;
    println!("detector at: {:?}", store.detector_path());

    let det = ObjectDetector::new(&store.detector_path(), "dfine-m-coco-v1")?;
    let img = image::open(&image)?.to_rgb8();
    let dets = det.detect(&img)?;
    println!("\n{} detections on {image:?}:", dets.len());
    for d in &dets {
        println!(
            "  {:<10} {:>5.1}%  {:<9} bbox(norm)=[{:.3},{:.3},{:.3},{:.3}]",
            d.label,
            d.confidence * 100.0,
            d.category,
            d.bbox[0],
            d.bbox[1],
            d.bbox[2],
            d.bbox[3]
        );
    }
    Ok(())
}
