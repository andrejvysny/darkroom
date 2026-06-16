//! Phase 0 spike: validate ort + CoreML + a license-clean D-FINE detector end-to-end.
//!
//! Usage: cargo run -p core-analyze --example detect_one -- [model.onnx] [image.(CR3|jpg|png)]
//! Defaults: model = ~/.cache/darkroom-models/dfine_s.onnx, image = first CR3 under library/.
//!
//! Prints: registered EPs (CoreML vs CPU), model I/O metadata, per-image latency, and detections
//! mapped to People / Animals / Vehicles buckets.

use anyhow::{anyhow, bail, Context, Result};
use image::imageops::FilterType;
use ort::ep::coreml::ComputeUnits;
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::time::Instant;

const INPUT: u32 = 640; // D-FINE / RT-DETR fixed input (preprocessor_config size).

#[rustfmt::skip]
const COCO: [&str; 80] = [
    "person","bicycle","car","motorbike","aeroplane","bus","train","truck","boat","traffic light",
    "fire hydrant","stop sign","parking meter","bench","bird","cat","dog","horse","sheep","cow",
    "elephant","bear","zebra","giraffe","backpack","umbrella","handbag","tie","suitcase","frisbee",
    "skis","snowboard","sports ball","kite","baseball bat","baseball glove","skateboard","surfboard",
    "tennis racket","bottle","wine glass","cup","fork","knife","spoon","bowl","banana","apple",
    "sandwich","orange","broccoli","carrot","hot dog","pizza","donut","cake","chair","sofa",
    "pottedplant","bed","diningtable","toilet","tvmonitor","laptop","mouse","remote","keyboard",
    "cell phone","microwave","oven","toaster","sink","refrigerator","book","clock","vase","scissors",
    "teddy bear","hair drier","toothbrush",
];

const ANIMALS: &[&str] = &[
    "bird", "cat", "dog", "horse", "sheep", "cow", "elephant", "bear", "zebra", "giraffe",
];
const VEHICLES: &[&str] = &["bicycle", "car", "motorbike", "motorcycle", "bus", "truck"];

fn category(label: &str) -> Option<&'static str> {
    if label == "person" {
        Some("People")
    } else if ANIMALS.contains(&label) {
        Some("Animals")
    } else if VEHICLES.contains(&label) {
        Some("Vehicles")
    } else {
        None
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
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

/// Decode to sRGB 8-bit RGB. RAW → embedded preview (sRGB); otherwise the image crate.
fn load_srgb(path: &Path) -> Result<image::RgbImage> {
    if is_raw(path) {
        let src = core_raw::source_from_path(path).with_context(|| format!("open {path:?}"))?;
        let img = core_raw::preview_image(&src).context("decode embedded preview")?;
        Ok(img.to_rgb8())
    } else {
        Ok(image::open(path)?.to_rgb8())
    }
}

fn default_image() -> Option<PathBuf> {
    let root = Path::new("library");
    if !root.exists() {
        return None;
    }
    walkdir(root).into_iter().find(|p| is_raw(p))
}

fn walkdir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

/// Build a session, trying CoreML (ANE) first with a hard error so we can *prove* it registered;
/// fall back to CPU if CoreML is unavailable.
fn build_session(model: &Path) -> Result<(Session, &'static str)> {
    let coreml = ort::ep::CoreML::default()
        .with_compute_units(ComputeUnits::All)
        .build()
        .error_on_failure();
    // `with_execution_providers` returns a builder-carrying error type, so map each step to anyhow.
    let attempt = Session::builder()
        .map_err(|e| anyhow!("{e}"))
        .and_then(|b| {
            b.with_execution_providers([coreml])
                .map_err(|e| anyhow!("{e}"))
        })
        .and_then(|mut b| b.commit_from_file(model).map_err(|e| anyhow!("{e}")));
    match attempt {
        Ok(s) => Ok((s, "CoreML")),
        Err(e) => {
            eprintln!("⚠ CoreML EP unavailable ({e}); falling back to CPU");
            let s = Session::builder()?.commit_from_file(model)?;
            Ok((s, "CPU"))
        }
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let model = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_cache().join("dfine_s.onnx"));
    let image = args
        .next()
        .map(PathBuf::from)
        .or_else(default_image)
        .context("no image given and no CR3 found under library/")?;

    if !model.exists() {
        bail!("model not found: {model:?}");
    }
    println!("model: {model:?}\nimage: {image:?}");

    // ---- preprocess: resize to 640x640 (exact, squashes aspect — matches RTDetrImageProcessor), /255 ----
    let rgb = load_srgb(&image)?;
    let (ow, oh) = (rgb.width(), rgb.height());
    let resized = image::imageops::resize(&rgb, INPUT, INPUT, FilterType::Triangle);
    let mut chw = vec![0f32; 3 * (INPUT * INPUT) as usize];
    let plane = (INPUT * INPUT) as usize;
    for (x, y, px) in resized.enumerate_pixels() {
        let i = (y * INPUT + x) as usize;
        chw[i] = px[0] as f32 / 255.0;
        chw[plane + i] = px[1] as f32 / 255.0;
        chw[2 * plane + i] = px[2] as f32 / 255.0;
    }

    // ---- session ----
    let (mut session, ep) = build_session(&model)?;
    println!("\nexecution provider: {ep}");
    println!("inputs:  {:#?}", session.inputs());
    println!("outputs: {:#?}", session.outputs());

    // ---- run (warmup + timed) ----
    let mk = || Tensor::from_array(([1usize, 3, INPUT as usize, INPUT as usize], chw.clone()));
    let _ = session.run(ort::inputs![mk()?])?; // warmup (CoreML compiles the graph)
    let runs = 5;
    let t0 = Instant::now();
    for _ in 0..runs {
        let _ = session.run(ort::inputs![mk()?])?; // outputs dropped each iter (borrow released)
    }
    let per = t0.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    println!("\nlatency: {per:.1} ms/image (avg of {runs}, after warmup)");
    let outputs = session.run(ort::inputs![mk()?])?; // final run for decoding

    // ---- locate logits (last dim 80) + boxes (last dim 4) by shape, run order-independent ----
    let mut logits = None;
    let mut boxes = None;
    for i in 0..outputs.len() {
        let arr = outputs[i].try_extract_array::<f32>()?;
        match arr.shape().last().copied() {
            Some(80) => logits = Some(arr),
            Some(4) => boxes = Some(arr),
            _ => {}
        }
    }
    let logits = logits.context("no [.,.,80] logits output")?;
    let boxes = boxes.context("no [.,.,4] boxes output")?;
    let nq = logits.shape()[1];

    // ---- decode (sigmoid per class, argmax, cxcywh→xyxy scaled to original) ----
    let thresh = 0.40f32;
    let mut dets: Vec<(String, f32, [f32; 4])> = Vec::new();
    for q in 0..nq {
        let (mut best_c, mut best_s) = (0usize, 0f32);
        for c in 0..80 {
            let s = sigmoid(logits[[0, q, c]]);
            if s > best_s {
                best_s = s;
                best_c = c;
            }
        }
        if best_s < thresh {
            continue;
        }
        let (cx, cy, w, h) = (
            boxes[[0, q, 0]],
            boxes[[0, q, 1]],
            boxes[[0, q, 2]],
            boxes[[0, q, 3]],
        );
        let bx = [
            (cx - w / 2.0) * ow as f32,
            (cy - h / 2.0) * oh as f32,
            (cx + w / 2.0) * ow as f32,
            (cy + h / 2.0) * oh as f32,
        ];
        dets.push((COCO[best_c].to_string(), best_s, bx));
    }
    dets.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());

    println!("\nsource {ow}x{oh}, {} detections ≥ {thresh}:", dets.len());
    let mut buckets = std::collections::BTreeMap::<&str, usize>::new();
    for (label, score, bx) in &dets {
        let cat = category(label).unwrap_or("(other)");
        if let Some(c) = category(label) {
            *buckets.entry(c).or_default() += 1;
        }
        println!(
            "  {:<14} {:>5.1}%  [{:.0},{:.0},{:.0},{:.0}]  {}",
            label,
            score * 100.0,
            bx[0],
            bx[1],
            bx[2],
            bx[3],
            cat
        );
    }
    println!("\nbuckets (target classes): {buckets:?}");
    Ok(())
}

fn dirs_cache() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cache/darkroom-models")
}
