//! Linear-probe trainer — embeds every hand-labeled image with the MobileCLIP-S1 vision encoder
//! (`Verifier::embed_full`, the same model the crop-verifier already loads) and fits a class-balanced
//! logistic-regression head per category (person / animal) on the 512-d feature. Prints the weights
//! as JSON to **stdout** (commit to `assets/presence_probe.json`); diagnostics to **stderr**.
//!
//! The fit standardizes features internally (per-dim mean/std) for fast convergence, then FOLDS the
//! standardization back into the weights so the runtime probe is a plain `sigmoid(w·x + b)` on the
//! raw L2-normalized embedding — no mean/std needed at inference. Honesty: AUC, the max-F1 threshold
//! `tau`, and CV-F1 are computed on 5-fold OUT-OF-FOLD predictions (not the training fit).
//!
//! Usage:
//!   cargo run -p core-analyze --example train_presence > crates/core-analyze/assets/presence_probe.json
//!   DB=/path/catalog.db MODELS=/path/models cargo run -p core-analyze --example train_presence

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use core_analyze::metrics::confusion_at;
use core_analyze::models::ModelStore;
use core_analyze::Verifier;
use core_db::rusqlite::{Connection, OpenFlags};
use core_library::{labeled_images, LabeledImage};
use image::imageops::FilterType;
use serde::Serialize;

const ANALYZE_EDGE: u32 = 1024;
const PRESENCE_VERSION: &str = "mobileclip-s1-probe-v1";
const FOLDS: usize = 5;

// ---- scaffolding (mirrors presence_eval) ----

fn app_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.andrejvysny.darkroom")
}
fn models_dir() -> PathBuf {
    std::env::var("MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app_data_dir().join("models"))
}
fn catalog_path() -> PathBuf {
    std::env::var("DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| app_data_dir().join("catalog.db"))
}
fn open_catalog(path: &Path) -> Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .or_else(|_| Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE))
        .with_context(|| format!("open catalog {}", path.display()))
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

// ---- logistic regression ----

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Per-dim (mean, std) over the training rows; std floored to avoid divide-by-zero on constant dims.
fn standardizer(x: &[Vec<f32>]) -> (Vec<f32>, Vec<f32>) {
    let (n, d) = (x.len() as f32, x[0].len());
    let mut mean = vec![0f32; d];
    for row in x {
        for j in 0..d {
            mean[j] += row[j];
        }
    }
    mean.iter_mut().for_each(|m| *m /= n);
    let mut std = vec![0f32; d];
    for row in x {
        for j in 0..d {
            let dz = row[j] - mean[j];
            std[j] += dz * dz;
        }
    }
    std.iter_mut().for_each(|s| *s = (*s / n).sqrt().max(1e-6));
    (mean, std)
}

fn standardize(row: &[f32], mean: &[f32], std: &[f32]) -> Vec<f32> {
    (0..row.len())
        .map(|j| (row[j] - mean[j]) / std[j])
        .collect()
}

/// Class-balanced logistic regression (batch GD + L2) in standardized space → `(w, b)`.
fn fit(z: &[Vec<f32>], y: &[bool], iters: usize, lr: f32, lambda: f32) -> (Vec<f32>, f32) {
    let (n, d) = (z.len(), z[0].len());
    let n_pos = y.iter().filter(|b| **b).count().max(1);
    let n_neg = (n - n_pos).max(1);
    // Inverse-frequency class weights (balance the skew), normalized so the mean weight ≈ 1.
    let cw_pos = n as f32 / (2.0 * n_pos as f32);
    let cw_neg = n as f32 / (2.0 * n_neg as f32);
    let sw: f32 = y.iter().map(|&yi| if yi { cw_pos } else { cw_neg }).sum();
    let (mut w, mut b) = (vec![0f32; d], 0f32);
    for _ in 0..iters {
        let (mut gw, mut gb) = (vec![0f32; d], 0f32);
        for (zi, &yi) in z.iter().zip(y) {
            let p = sigmoid(b + dot(&w, zi));
            let err = if yi { cw_pos } else { cw_neg } * (p - if yi { 1.0 } else { 0.0 });
            for j in 0..d {
                gw[j] += err * zi[j];
            }
            gb += err;
        }
        for j in 0..d {
            w[j] -= lr * (gw[j] / sw + lambda * w[j]);
        }
        b -= lr * (gb / sw);
    }
    (w, b)
}

/// ROC-AUC via the Mann-Whitney U statistic (mean rank of positives). `None` if a class is empty.
fn roc_auc(scored: &[(f32, bool)]) -> Option<f32> {
    let n_pos = scored.iter().filter(|(_, y)| *y).count();
    let n_neg = scored.len() - n_pos;
    if n_pos == 0 || n_neg == 0 {
        return None;
    }
    let mut idx: Vec<usize> = (0..scored.len()).collect();
    idx.sort_by(|&a, &b| {
        scored[a]
            .0
            .partial_cmp(&scored[b].0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Average ranks (1-based), handling ties by sharing the mean rank across the tied run.
    let mut ranks = vec![0f64; scored.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i + 1;
        while j < idx.len() && scored[idx[j]].0.to_bits() == scored[idx[i]].0.to_bits() {
            j += 1;
        }
        let avg = ((i + 1 + j) as f64) / 2.0; // mean of ranks (i+1)..=j
        for &k in &idx[i..j] {
            ranks[k] = avg;
        }
        i = j;
    }
    let sum_pos: f64 = scored
        .iter()
        .zip(&ranks)
        .filter(|((_, y), _)| *y)
        .map(|(_, r)| *r)
        .sum();
    let u = sum_pos - (n_pos * (n_pos + 1)) as f64 / 2.0;
    Some((u / (n_pos as f64 * n_neg as f64)) as f32)
}

/// Max-F1 probability threshold over `scored` (sweep 0.01..=0.99), returning `(tau, f1)`.
fn max_f1(scored: &[(f32, bool)]) -> (f32, f32) {
    let mut best = (0.5f32, 0f32);
    for i in 1..100 {
        let tau = i as f32 / 100.0;
        let f1 = confusion_at(scored, tau).f1().unwrap_or(0.0);
        if f1 > best.1 {
            best = (tau, f1);
        }
    }
    best
}

// ---- output ----

#[derive(Serialize)]
struct Head {
    /// Folded weights operating directly on the raw L2-normalized embedding.
    w: Vec<f32>,
    b: f32,
    /// Baked max-F1 decision threshold on `sigmoid(w·x + b)` (from out-of-fold predictions).
    tau: f32,
    cv_auc: f32,
    cv_f1: f32,
    n_pos: usize,
    n_neg: usize,
}
#[derive(Serialize)]
struct Probe {
    model_version: String,
    dim: usize,
    person: Head,
    animal: Head,
}

/// One per-image cross-validated OOF prediction row (for the offline fusion join; `None` = that
/// category was unlabeled for the image).
#[derive(Serialize)]
struct OofRow {
    image_id: i64,
    oof_person: Option<f32>,
    oof_animal: Option<f32>,
}

/// Fold standardization (mean,std) back into weights so runtime scores the raw embedding.
fn fold(w: &[f32], b: f32, mean: &[f32], std: &[f32]) -> (Vec<f32>, f32) {
    let wf: Vec<f32> = (0..w.len()).map(|j| w[j] / std[j]).collect();
    let bf = b - (0..w.len()).map(|j| w[j] * mean[j] / std[j]).sum::<f32>();
    (wf, bf)
}

const ITERS: usize = 1500;
const LR: f32 = 0.5;
const LAMBDA: f32 = 1e-2;

/// Fit one category: 5-fold OOF predictions for honest AUC/tau/F1, then a final fit on all rows with
/// standardization folded into the returned weights. Also returns the per-row cross-validated OOF
/// probability (aligned to `feats` order) for offline fusion measurement.
fn train_head(name: &str, feats: &[Vec<f32>], labels: &[bool]) -> (Head, Vec<f32>) {
    let n_pos = labels.iter().filter(|b| **b).count();
    let n_neg = labels.len() - n_pos;

    // Out-of-fold predictions, kept aligned to input index (deterministic split: index % FOLDS).
    let mut oof_idx = vec![0f32; feats.len()];
    for f in 0..FOLDS {
        let tr: Vec<usize> = (0..feats.len()).filter(|i| i % FOLDS != f).collect();
        let te: Vec<usize> = (0..feats.len()).filter(|i| i % FOLDS == f).collect();
        let tr_x: Vec<Vec<f32>> = tr.iter().map(|&i| feats[i].clone()).collect();
        let tr_y: Vec<bool> = tr.iter().map(|&i| labels[i]).collect();
        let (mean, std) = standardizer(&tr_x);
        let z: Vec<Vec<f32>> = tr_x.iter().map(|r| standardize(r, &mean, &std)).collect();
        let (w, b) = fit(&z, &tr_y, ITERS, LR, LAMBDA);
        for &i in &te {
            let zi = standardize(&feats[i], &mean, &std);
            oof_idx[i] = sigmoid(b + dot(&w, &zi));
        }
    }
    let oof: Vec<(f32, bool)> = oof_idx.iter().zip(labels).map(|(&p, &y)| (p, y)).collect();
    let cv_auc = roc_auc(&oof).unwrap_or(f32::NAN);
    let (tau, cv_f1) = max_f1(&oof);

    // Final fit on all rows; fold standardization into the weights.
    let (mean, std) = standardizer(feats);
    let z: Vec<Vec<f32>> = feats.iter().map(|r| standardize(r, &mean, &std)).collect();
    let (w, b) = fit(&z, labels, ITERS, LR, LAMBDA);
    let (wf, bf) = fold(&w, b, &mean, &std);

    eprintln!(
        "{name:>7}: n={} (pos {n_pos}, neg {n_neg})  CV ROC-AUC={cv_auc:.3}  max-F1 tau={tau:.2} F1={cv_f1:.3}",
        feats.len()
    );
    (
        Head {
            w: wf,
            b: bf,
            tau,
            cv_auc,
            cv_f1,
            n_pos,
            n_neg,
        },
        oof_idx,
    )
}

fn main() -> Result<()> {
    let store = ModelStore::new(models_dir());
    let (v_vis, v_txt, v_tok) = store.verifier_paths();
    let verifier = Verifier::new(&v_vis, &v_txt, &v_tok).context("load verifier")?;

    let db_path = catalog_path();
    let conn = open_catalog(&db_path)?;
    let labeled = labeled_images(&conn).context("query labeled_images")?;
    eprintln!(
        "catalog: {}  | {} labeled images — embedding…",
        db_path.display(),
        labeled.len()
    );

    // Embed every labeled image once; bucket into per-category (id, feature, label) by tri-state.
    let (mut p_x, mut p_y, mut p_ids) = (Vec::new(), Vec::new(), Vec::new());
    let (mut a_x, mut a_y, mut a_ids) = (Vec::new(), Vec::new(), Vec::new());
    let (mut ok, mut failed) = (0usize, 0usize);
    for LabeledImage {
        id,
        path,
        person,
        animal,
    } in &labeled
    {
        let img = match decode_srgb(Path::new(path)) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("skip {path} (decode: {e:#})");
                failed += 1;
                continue;
            }
        };
        let emb = match verifier.embed_full(&img) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("skip {path} (embed: {e:#})");
                failed += 1;
                continue;
            }
        };
        if let Some(y) = person {
            p_x.push(emb.clone());
            p_y.push(*y);
            p_ids.push(*id);
        }
        if let Some(y) = animal {
            a_x.push(emb.clone());
            a_y.push(*y);
            a_ids.push(*id);
        }
        ok += 1;
        if ok % 50 == 0 {
            eprintln!("  embedded {ok}…");
        }
    }
    eprintln!("embedded {ok}, skipped {failed}");
    anyhow::ensure!(!p_x.is_empty() && !a_x.is_empty(), "no labeled features");

    let dim = p_x[0].len();
    let (person, p_oof) = train_head("person", &p_x, &p_y);
    let (animal, a_oof) = train_head("animal", &a_x, &a_y);
    let probe = Probe {
        model_version: PRESENCE_VERSION.to_string(),
        dim,
        person,
        animal,
    };
    println!("{}", serde_json::to_string_pretty(&probe)?);

    // Optional per-image cross-validated OOF dump for offline fusion measurement (no behavior change).
    if let Ok(out_path) = std::env::var("OOF_OUT") {
        use std::collections::BTreeMap;
        let mut m: BTreeMap<i64, (Option<f32>, Option<f32>)> = BTreeMap::new();
        for (id, p) in p_ids.iter().zip(&p_oof) {
            m.entry(*id).or_default().0 = Some(*p);
        }
        for (id, p) in a_ids.iter().zip(&a_oof) {
            m.entry(*id).or_default().1 = Some(*p);
        }
        let mut buf = String::new();
        for (id, (oof_person, oof_animal)) in &m {
            buf.push_str(&serde_json::to_string(&OofRow {
                image_id: *id,
                oof_person: *oof_person,
                oof_animal: *oof_animal,
            })?);
            buf.push('\n');
        }
        std::fs::write(&out_path, buf).with_context(|| format!("write {out_path}"))?;
        eprintln!("OOF dump → {out_path} ({} images)", m.len());
    }
    Ok(())
}
