//! Validate the real feature-compute path on one image: decode (linear preview + as-shot WB) →
//! `compute_features`. Mirrors what the backfill pass does per image.
//!
//! Usage: cargo run -p core-library --example features_one -- [image.CR3]

use std::path::Path;

type Err = Box<dyn std::error::Error>;

fn main() -> Result<(), Err> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "library/nature/_55A4115.CR3".to_string());
    println!("image: {path}");
    let src = core_raw::source_from_path(Path::new(&path))?;
    let lin = core_raw::develop_linear_preview(&src)?;
    let wb = core_raw::as_shot_wb(&src).unwrap_or([1.0; 4]);
    println!("linear preview: {}×{}", lin.width, lin.height);
    println!("as-shot wb_coeffs: {wb:?}");

    let f = core_library::compute_features(&lin, wb);
    println!(
        "wb_as_shot_rg={:.4} wb_as_shot_bg={:.4}",
        f.wb_as_shot_rg, f.wb_as_shot_bg
    );
    println!("mean_log_luma={:.4}", f.mean_log_luma);
    println!("clip_hi={:.4} clip_lo={:.4}", f.clip_hi, f.clip_lo);
    println!("dynamic_range_ev={:.3}", f.dynamic_range_ev);
    println!("sharpness={:.6}", f.sharpness);
    println!(
        "hist_luma bins={} (sum={:.3}), hist_logchroma bins={} (sum={:.3})",
        f.hist_luma.len(),
        f.hist_luma.iter().sum::<f32>(),
        f.hist_logchroma.len(),
        f.hist_logchroma.iter().sum::<f32>(),
    );
    Ok(())
}
