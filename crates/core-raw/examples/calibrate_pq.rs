//! Spike-S2 PQ brightness calibration: compare a same-capture CR3+HIF pair (RAW+HEIF simultaneous
//! recording on the R7) and measure the develop-brightness offset between the two decode paths.
//! Run on the dev machine once real fixtures exist:
//!   cargo run -p core-raw --example calibrate_pq <pair.CR3> <pair.HIF>
//!
//! Prints the mid-tone geometric-mean luminance ratio as ΔEV and the `HDR_DIFFUSE_WHITE_NITS`
//! value that would zero it. Decision rule (see `core-raw/src/color.rs`): keep 203 if |ΔEV| ≤ 0.25,
//! otherwise adopt the suggested constant (rounded) and record the measurement in `color.rs` docs.

use std::path::Path;

/// Luminance row of ProPhoto(D50)→XYZ — the Y contribution of linear-ProPhoto RGB.
const PP_LUMA: [f64; 3] = [0.2880402, 0.7118741, 0.0000857];

fn midtone_geomean(img: &core_raw::LinearImage, mask: impl Fn(f64) -> bool) -> (f64, usize) {
    // Center crop 50% to dodge vignetting differences between the two paths.
    let (w, h) = (img.width as usize, img.height as usize);
    let (x0, x1) = (w / 4, w * 3 / 4);
    let (y0, y1) = (h / 4, h * 3 / 4);
    let mut log_sum = 0f64;
    let mut n = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * w + x) * 3;
            let yl = PP_LUMA[0] * img.data[i] as f64
                + PP_LUMA[1] * img.data[i + 1] as f64
                + PP_LUMA[2] * img.data[i + 2] as f64;
            if mask(yl) {
                log_sum += yl.max(1e-8).ln();
                n += 1;
            }
        }
    }
    ((log_sum / n.max(1) as f64).exp(), n)
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let (Some(cr3), Some(hif)) = (args.next(), args.next()) else {
        eprintln!("usage: calibrate_pq <pair.CR3> <pair.HIF>");
        return Ok(());
    };

    let dec = |p: &str| -> anyhow::Result<core_raw::LinearImage> {
        let src = core_raw::source_from_path(Path::new(p))?;
        Ok(core_raw::develop_linear(&src)?)
    };
    let raw = dec(&cr3)?;
    let heif = dec(&hif)?;
    println!(
        "CR3: {}x{}   HIF: {}x{}",
        raw.width, raw.height, heif.width, heif.height
    );

    // Mid-tones only (Y ∈ [0.02, 0.5] in each path's own scale) — clipped/near-black pixels would
    // bias the ratio.
    let (g_raw, n_raw) = midtone_geomean(&raw, |y| (0.02..=0.5).contains(&y));
    let (g_hif, n_hif) = midtone_geomean(&heif, |y| (0.02..=0.5).contains(&y));
    println!("geomean Y: CR3 {g_raw:.5} ({n_raw} px)   HIF {g_hif:.5} ({n_hif} px)");

    let ratio = g_hif / g_raw;
    let dev = ratio.log2();
    let current = core_raw::HDR_DIFFUSE_WHITE_NITS;
    println!("HIF/CR3 ratio {ratio:.4} = {dev:+.3} EV");
    println!(
        "suggested HDR_DIFFUSE_WHITE_NITS = {:.0}  (current {current:.0})",
        current * ratio
    );
    if dev.abs() <= 0.25 {
        println!("|ΔEV| ≤ 0.25 → keep {current:.0}.");
    } else {
        println!("|ΔEV| > 0.25 → adopt the suggested constant in core-raw/src/color.rs and note the measurement.");
    }
    Ok(())
}
