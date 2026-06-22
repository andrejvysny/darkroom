//! Calibration harness for the scene-referred base tone operator's baseline gain.
//!
//! The ACR-fit base curve (`params::acr_curve`) maps scene-linear mid-grey 0.18 → ~0.388 display
//! (the canonical ACR mid-grey). To make our DEFAULT render match ACR brightness, a correctly-exposed
//! mid-grey in our `develop_linear` ProPhoto buffer must arrive at the curve's 0.18 input. This tool
//! reports where mid-grey actually lands (`g0`) so we can set `baseline_gain b = 0.18 / g0`.
//!
//! Run: `cargo run -p core-pipeline --example measure_midgrey [FILE.CR3 ...]`
//! (no GPU needed). Pass several correctly-exposed frames and average their `g0`.

use core_raw::{develop_linear, source_from_path};
use std::path::PathBuf;

// Same luminance weights the develop shader uses (Rec.709 on the ProPhoto buffer; for neutral grey
// R=G=B so the exact weights are irrelevant — they only shape the scene-key estimate).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

fn cr3_in_library() -> Vec<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../library/2026/2026-06-06");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v
}

fn pct(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((p * (sorted.len() - 1) as f32).round() as usize).min(sorted.len() - 1);
    sorted[i]
}

fn main() -> anyhow::Result<()> {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let files = if args.is_empty() {
        cr3_in_library()
    } else {
        args
    };
    if files.is_empty() {
        eprintln!("no CR3 files (pass paths, or commit a fixture under library/2026/2026-06-06)");
        return Ok(());
    }

    let mut g0_sum = 0.0f64;
    let mut g0_n = 0u32;
    for path in &files {
        let lin = match source_from_path(path)
            .map_err(|e| e.to_string())
            .and_then(|s| develop_linear(&s).map_err(|e| e.to_string()))
        {
            Ok(l) => l,
            Err(e) => {
                eprintln!("skip {}: {e}", path.display());
                continue;
            }
        };
        let px = (lin.data.len() / 3) as f32;
        let mut lum: Vec<f32> = Vec::with_capacity(lin.data.len() / 3);
        let mut log_sum = 0.0f64; // for geometric mean (scene "key")
        let mut over1 = 0u64; // headroom usage
        for c in lin.data.chunks_exact(3) {
            let y = (LUMA[0] * c[0] + LUMA[1] * c[1] + LUMA[2] * c[2]).max(0.0);
            if y > 1.0 {
                over1 += 1;
            }
            log_sum += (y.max(1e-6) as f64).ln();
            lum.push(y);
        }
        lum.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let geo = (log_sum / px as f64).exp() as f32; // geometric-mean luminance = scene key
        let median = pct(&lum, 0.5);
        let mean = lum.iter().sum::<f32>() / px;

        // The geometric mean is the classic mid-grey/key estimator for an average-keyed scene.
        let g0 = geo;
        g0_sum += g0 as f64;
        g0_n += 1;

        println!("── {}", path.display());
        println!(
            "   {}x{}  geomean(key)={geo:.5}  median={median:.5}  mean={mean:.5}",
            lin.width, lin.height
        );
        println!(
            "   p10={:.5} p25={:.5} p50={:.5} p75={:.5} p90={:.5} p99={:.5}  >1.0: {:.2}%",
            pct(&lum, 0.10),
            pct(&lum, 0.25),
            pct(&lum, 0.50),
            pct(&lum, 0.75),
            pct(&lum, 0.90),
            pct(&lum, 0.99),
            100.0 * over1 as f32 / px,
        );
        println!(
            "   → if this is correctly exposed: g0≈{g0:.5}, baseline_gain b = 0.18/g0 = {:.4}",
            0.18 / g0
        );
    }

    if g0_n > 0 {
        let g0_avg = (g0_sum / g0_n as f64) as f32;
        println!(
            "\n=== AVERAGE over {g0_n} frame(s): g0≈{g0_avg:.5} → baseline_gain b = {:.4} ===",
            0.18 / g0_avg
        );
        println!("(Seed `DevelopParams::BASELINE_GAIN` with this, then fine-tune by visual QA against ACR.)");
    }
    Ok(())
}
