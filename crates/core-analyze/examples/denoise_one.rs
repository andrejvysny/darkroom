//! End-to-end validation of the raw-domain denoise rail WITHOUT any ML model:
//! decode R7 CR3 → pack Bayer → CPU bilateral (behind the `MosaicDenoiser` seam) → unpack → write the
//! denoised mosaic back → unchanged develop → linear RGB. Proves the whole integration
//! (`develop_linear_denoised` with `denoise::{pack,tile_and_blend,unpack}`) on real sensor data. Writes PNGs to /tmp
//! and prints a smoothness metric (mean |Laplacian|) so the noise reduction is measurable, not assumed.
//!
//! Run: `cargo run -p core-analyze --example denoise_one`

use core_analyze::denoise::CpuDenoiser;
use core_raw::{DenoiseOutput, LinearImage};
use std::path::PathBuf;

fn find_cr3() -> Option<PathBuf> {
    for rel in ["library/2026/2026-06-06", "library", "DATA/2026/2026-06-06"] {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut hits: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("cr3"))
                    .unwrap_or(false)
            })
            .collect();
        hits.sort();
        if let Some(p) = hits.into_iter().next() {
            return Some(p);
        }
    }
    None
}

/// Mean absolute 4-neighbor Laplacian over the luma of a linear image — a scale for high-frequency
/// content (noise + detail). Denoise should lower it.
fn mean_abs_laplacian(img: &LinearImage) -> f64 {
    let (w, h) = (img.width as usize, img.height as usize);
    let luma = |x: usize, y: usize| -> f32 {
        let i = (y * w + x) * 3;
        0.299 * img.data[i] + 0.587 * img.data[i + 1] + 0.114 * img.data[i + 2]
    };
    let mut sum = 0f64;
    let mut n = 0u64;
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            let lap = 4.0 * luma(x, y)
                - luma(x - 1, y)
                - luma(x + 1, y)
                - luma(x, y - 1)
                - luma(x, y + 1);
            sum += lap.abs() as f64;
            n += 1;
        }
    }
    if n == 0 {
        0.0
    } else {
        sum / n as f64
    }
}

/// Crude linear→sRGB, clamp, 8-bit — just enough to eyeball noise (real color comes from the GPU develop).
fn save_png(img: &LinearImage, path: &str) {
    let mut buf = image::RgbImage::new(img.width, img.height);
    for (px, chunk) in buf.pixels_mut().zip(img.data.chunks_exact(3)) {
        let enc = |v: f32| -> u8 {
            let v = v.clamp(0.0, 1.0);
            let s = if v <= 0.0031308 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            (s * 255.0).round() as u8
        };
        *px = image::Rgb([enc(chunk[0]), enc(chunk[1]), enc(chunk[2])]);
    }
    buf.save(path).expect("write png");
}

fn main() {
    let Some(path) = find_cr3() else {
        eprintln!("No CR3 fixture found under library/ — skipping.");
        return;
    };
    println!("Denoising {}", path.display());
    let src = core_raw::source_from_path(&path).expect("open source");

    // Prefer the embedded neural PMRID model; fall back to the classical CPU denoiser when absent.
    let t = std::time::Instant::now();
    let DenoiseOutput { clean, denoised } = match core_analyze::denoise::try_pmrid() {
        Some(pmrid) => {
            println!("  denoiser: neural PMRID (embedded)");
            core_raw::develop_linear_denoised(&src, &pmrid)
        }
        None => {
            println!("  denoiser: CPU bilateral (no embedded model)");
            core_raw::develop_linear_denoised(&src, &CpuDenoiser { strength: 0.7 })
        }
    }
    .expect("develop_linear_denoised");
    let denoised = denoised.expect("R7 is RGB Bayer → denoised output expected");
    println!(
        "  {}x{}  ({:.2}s for clean+denoise develop)",
        clean.width,
        clean.height,
        t.elapsed().as_secs_f32()
    );

    let mut diff = 0f64;
    for (a, b) in clean.data.iter().zip(denoised.data.iter()) {
        diff += (a - b).abs() as f64;
    }
    diff /= clean.data.len() as f64;
    let lap_clean = mean_abs_laplacian(&clean);
    let lap_den = mean_abs_laplacian(&denoised);
    println!("  mean |clean-denoised|      = {diff:.5}");
    println!("  mean |Laplacian| clean     = {lap_clean:.5}");
    println!("  mean |Laplacian| denoised  = {lap_den:.5}");
    println!(
        "  high-freq reduced by        = {:.1}%",
        (1.0 - lap_den / lap_clean.max(1e-9)) * 100.0
    );

    save_png(&clean, "/tmp/denoise_clean.png");
    save_png(&denoised, "/tmp/denoise_denoised.png");
    println!("  wrote /tmp/denoise_clean.png and /tmp/denoise_denoised.png");

    assert!(diff > 0.0, "denoise made no change — rail is broken");
    assert!(
        lap_den < lap_clean,
        "denoise did not reduce high-frequency content"
    );
    println!("OK: denoise rail verified end-to-end on real sensor data.");
}
