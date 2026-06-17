//! Per-image MODEL INPUTS for future AI (lighting normalization, best-shot, dedup) — computed once
//! from the linear develop image + as-shot WB, stored (overwrite-in-place) in `image_features`.
//! Distinct from the event log (labels) and AI outputs. Deterministic from the RAW, so backfillable.

use core_db::rusqlite::{params, Connection, OptionalExtension};
use core_raw::LinearImage;
use serde::Serialize;

use crate::error::LibError;

const FEATURE_EDGE: u32 = 512; // normalize size so sharpness/histograms are comparable across images
const LUMA_BINS: usize = 256;
const CHROMA_BINS: usize = 32; // 32×32 log-chroma histogram
const CHROMA_RANGE: f32 = 2.0; // log(r/g), log(b/g) clamp range
const CLIP_HI: f32 = 0.99;
const CLIP_LO: f32 = 0.005;

/// Computed per-image features. Histograms are normalized to fractions (sum≈1) for scale-invariance.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageFeatures {
    pub wb_as_shot_rg: f32,
    pub wb_as_shot_bg: f32,
    pub hist_logchroma: Vec<f32>, // CHROMA_BINS²
    pub hist_luma: Vec<f32>,      // LUMA_BINS
    pub mean_log_luma: f32,
    pub clip_hi: f32,
    pub clip_lo: f32,
    pub dynamic_range_ev: f32,
    pub sharpness: f32,
}

/// Compute features from a linear-RGB image + as-shot WB coeffs `[r,g,b,g2]`.
pub fn compute_features(lin: &LinearImage, wb: [f32; 4]) -> ImageFeatures {
    let g = if wb[1] != 0.0 { wb[1] } else { 1.0 };
    let wb_as_shot_rg = wb[0] / g;
    let wb_as_shot_bg = wb[2] / g;

    let small = lin.downscaled(FEATURE_EDGE);
    let (w, h) = (small.width as usize, small.height as usize);
    let n = (w * h).max(1);

    let mut hist_luma = vec![0f32; LUMA_BINS];
    let mut hist_chroma = vec![0f32; CHROMA_BINS * CHROMA_BINS];
    let mut luma = vec![0f32; w * h];
    let mut sum_log = 0f64;
    let (mut clip_hi, mut clip_lo) = (0u32, 0u32);

    for (i, px) in small.data.chunks_exact(3).enumerate() {
        let r = px[0].max(0.0);
        let gg = px[1].max(0.0);
        let b = px[2].max(0.0);
        // Rec.709-ish luma (approximate; working space is linear ProPhoto).
        let y = 0.2126 * r + 0.7152 * gg + 0.0722 * b;
        luma[i] = y;
        sum_log += ((y + 1e-4) as f64).ln();
        if r >= CLIP_HI || gg >= CLIP_HI || b >= CLIP_HI {
            clip_hi += 1;
        }
        if y <= CLIP_LO {
            clip_lo += 1;
        }
        let yb = ((y.clamp(0.0, 1.0) * (LUMA_BINS as f32 - 1.0)) as usize).min(LUMA_BINS - 1);
        hist_luma[yb] += 1.0;
        // Log-chroma (needs a non-zero green).
        if gg > 1e-5 {
            let lr = (r.max(1e-5) / gg).ln().clamp(-CHROMA_RANGE, CHROMA_RANGE);
            let lb = (b.max(1e-5) / gg).ln().clamp(-CHROMA_RANGE, CHROMA_RANGE);
            let cr = chroma_bin(lr);
            let cb = chroma_bin(lb);
            hist_chroma[cr * CHROMA_BINS + cb] += 1.0;
        }
    }

    let nf = n as f32;
    for v in hist_luma.iter_mut() {
        *v /= nf;
    }
    let chroma_total: f32 = hist_chroma.iter().sum();
    if chroma_total > 0.0 {
        for v in hist_chroma.iter_mut() {
            *v /= chroma_total;
        }
    }

    ImageFeatures {
        wb_as_shot_rg,
        wb_as_shot_bg,
        hist_logchroma: hist_chroma,
        hist_luma: hist_luma.clone(),
        mean_log_luma: (sum_log / n as f64) as f32,
        clip_hi: clip_hi as f32 / nf,
        clip_lo: clip_lo as f32 / nf,
        dynamic_range_ev: dynamic_range_ev(&hist_luma),
        sharpness: laplacian_variance(&luma, w, h),
    }
}

fn chroma_bin(v: f32) -> usize {
    let t = (v + CHROMA_RANGE) / (2.0 * CHROMA_RANGE); // → [0,1]
    ((t * (CHROMA_BINS as f32 - 1.0)) as usize).min(CHROMA_BINS - 1)
}

/// EV between the 0.5% and 99.5% luma percentiles, from the normalized luma histogram.
fn dynamic_range_ev(hist_luma: &[f32]) -> f32 {
    let pct = |target: f32| -> f32 {
        let mut acc = 0.0;
        for (i, &v) in hist_luma.iter().enumerate() {
            acc += v;
            if acc >= target {
                return i as f32 / (LUMA_BINS as f32 - 1.0);
            }
        }
        1.0
    };
    let lo = pct(0.005).max(1.0 / 4096.0);
    let hi = pct(0.995).max(lo + 1.0 / 4096.0);
    (hi / lo).log2().clamp(0.0, 24.0)
}

/// Variance of the 4-neighbour Laplacian over the luma plane (focus/quality proxy).
fn laplacian_variance(luma: &[f32], w: usize, h: usize) -> f32 {
    if w < 3 || h < 3 {
        return 0.0;
    }
    let mut vals = Vec::with_capacity((w - 2) * (h - 2));
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = luma[y * w + x];
            let lap = luma[y * w + x - 1]
                + luma[y * w + x + 1]
                + luma[(y - 1) * w + x]
                + luma[(y + 1) * w + x]
                - 4.0 * c;
            vals.push(lap);
        }
    }
    let m = vals.iter().sum::<f32>() / vals.len() as f32;
    vals.iter().map(|v| (v - m) * (v - m)).sum::<f32>() / vals.len() as f32
}

fn f32_le_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Upsert one image's features (overwrite-in-place).
pub fn set_image_features(
    conn: &Connection,
    image_id: i64,
    f: &ImageFeatures,
    now: i64,
) -> Result<(), LibError> {
    conn.execute(
        "INSERT INTO image_features
           (image_id, wb_as_shot_rg, wb_as_shot_bg, hist_logchroma, hist_luma, mean_log_luma,
            clip_hi, clip_lo, dynamic_range_ev, sharpness, computed_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
         ON CONFLICT(image_id) DO UPDATE SET
           wb_as_shot_rg=?2, wb_as_shot_bg=?3, hist_logchroma=?4, hist_luma=?5, mean_log_luma=?6,
           clip_hi=?7, clip_lo=?8, dynamic_range_ev=?9, sharpness=?10, computed_at=?11",
        params![
            image_id,
            f.wb_as_shot_rg,
            f.wb_as_shot_bg,
            f32_le_bytes(&f.hist_logchroma),
            f32_le_bytes(&f.hist_luma),
            f.mean_log_luma,
            f.clip_hi,
            f.clip_lo,
            f.dynamic_range_ev,
            f.sharpness,
            now,
        ],
    )?;
    Ok(())
}

/// `(image_id, path)` for present images that have no features row yet (backfill work-list).
pub fn images_missing_features(conn: &Connection) -> Result<Vec<(i64, String)>, LibError> {
    let mut stmt = conn.prepare(
        "SELECT i.id, i.path FROM images i
         LEFT JOIN image_features f ON f.image_id = i.id
         WHERE i.status='present' AND f.image_id IS NULL",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// True if an image already has a features row.
pub fn has_features(conn: &Connection, image_id: i64) -> Result<bool, LibError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM image_features WHERE image_id=?1",
            [image_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}
