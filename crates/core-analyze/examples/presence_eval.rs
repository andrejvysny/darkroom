//! Label-driven presence eval — runs the REAL detector pipeline (`ObjectDetector` + `MegaDetector` +
//! shared MobileCLIP `Verifier`) over every hand-labeled image in the catalog and emits diagnostic
//! pre-gate scores, so the thresholds can be calibrated against ground truth instead of 4 hardcoded
//! frames. Mirrors the production decode path (`core_raw::preview_image` → ≤1024 Triangle) and the
//! `registry()` wiring in `src-tauri/src/analysis.rs`.
//!
//! Output: one JSONL row per image to **stdout**; a per-category metrics summary to **stderr**.
//! Pipe stdout into `presence_tune` to grid-search the max-F1 operating points.
//!
//! Usage:
//!   cargo run -p core-analyze --example presence_eval > /tmp/p.jsonl
//!   NO_VERIFY=1 cargo run -p core-analyze --example presence_eval > /tmp/p_nv.jsonl   # ablation
//!   MD_SIZE=640 DARKROOM_MD_CPU=1 cargo run -p core-analyze --example presence_eval   # quick / CPU
//!   DB=/path/catalog.db MODELS=/path/models cargo run -p core-analyze --example presence_eval
//!
//! Env: `DB` catalog path, `MODELS` models dir, `MD_SIZE` (640|1280, default 1280), `NO_VERIFY=1`
//! drops the CLIP gate, plus the detector/MD/verifier `DARKROOM_*` sweep overrides.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use core_analyze::metrics::{pr_auc, Confusion};
use core_analyze::models::ModelStore;
use core_analyze::{MegaDetector, ObjectDetector, RawScore, Verifier};
use core_db::rusqlite::{Connection, OpenFlags};
use core_library::labeled_images;
use image::imageops::FilterType;
use serde::Serialize;

const ANALYZE_EDGE: u32 = 1024; // mirror src-tauri/src/analysis.rs

fn app_data_dir() -> PathBuf {
    // Matches Tauri's `app_data_dir` on every OS (macOS: ~/Library/Application Support).
    dirs::data_dir()
        .expect("no data dir")
        .join("com.andrejvysny.darkroom")
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

/// Open the catalog without migrating (unlike `core_db::Db::open`). Prefer strictly read-only; fall
/// back to read-write (no CREATE) because a WAL-mode DB with a live `-wal` file can't be opened
/// read-only (SQLite must write the `-shm` wal-index). We only ever issue SELECTs.
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

/// Production decode: RAW embedded preview (or a plain image) → sRGB → downscale to ANALYZE_EDGE.
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

/// One JSONL output row (snake_case fields; `Option` → JSON `null`).
#[derive(Serialize)]
struct EvalRow<'a> {
    image_id: i64,
    path: &'a str,
    person_label: Option<bool>,
    animal_label: Option<bool>,
    person_raw: f32,
    person_vprob: Option<f32>,
    person_gated: bool,
    animal_raw: f32,
    animal_vprob: Option<f32>,
    animal_gated: bool,
}

/// Per-category accumulator: `(raw_score, current_gate_pred, label)` for each labeled image.
#[derive(Default)]
struct Cat {
    rows: Vec<(f32, bool, bool)>,
}

impl Cat {
    fn push(&mut self, raw: f32, gated: bool, label: Option<bool>) {
        if let Some(y) = label {
            self.rows.push((raw, gated, y));
        }
    }

    fn summary(&self, name: &str) {
        let n_pos = self.rows.iter().filter(|(_, _, y)| *y).count();
        let n_neg = self.rows.len() - n_pos;
        let mut c = Confusion::default();
        for &(_, pred, y) in &self.rows {
            match (pred, y) {
                (true, true) => c.tp += 1,
                (true, false) => c.fp += 1,
                (false, true) => c.fn_ += 1,
                (false, false) => c.tn += 1,
            }
        }
        let pr: Vec<(f32, bool)> = self.rows.iter().map(|&(s, _, y)| (s, y)).collect();
        let f = |o: Option<f32>| o.map(|v| format!("{v:.3}")).unwrap_or_else(|| "n/a".into());
        eprintln!(
            "{name:>8}: n={:<4} (pos {n_pos}, neg {n_neg})  CURRENT gate P={} R={} F1={}  \
             (tp {} fp {} fn {} tn {})  raw PR-AUC={}",
            self.rows.len(),
            f(c.precision()),
            f(c.recall()),
            f(c.f1()),
            c.tp,
            c.fp,
            c.fn_,
            c.tn,
            f(pr_auc(&pr)),
        );
    }
}

fn main() -> Result<()> {
    let store = ModelStore::new(models_dir());
    let md_size: u32 = std::env::var("MD_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1280);

    // Shared CLIP verifier (mirrors registry()); NO_VERIFY=1 runs the detector gates only.
    let verifier = if std::env::var("NO_VERIFY").is_err() {
        let (v_vis, v_txt, v_tok) = store.verifier_paths();
        eprintln!("verifier: MobileCLIP-S1 attached");
        Some(Arc::new(
            Verifier::new(&v_vis, &v_txt, &v_tok).context("load verifier")?,
        ))
    } else {
        eprintln!("verifier: none (detector gates only)");
        None
    };

    let mut detector =
        ObjectDetector::new(&store.detector_path(), "eval").context("load D-FINE")?;
    let mut mega =
        MegaDetector::new(&store.animal_detector_path(), "eval", md_size).context("load MD")?;
    if let Some(v) = &verifier {
        detector = detector.with_verifier(v.clone());
        mega = mega.with_verifier(v.clone());
    }
    eprintln!(
        "models: {}  | MD size {md_size}{}",
        store.detector_path().display(),
        if std::env::var_os("DARKROOM_MD_CPU").is_some() {
            " (CPU)"
        } else {
            " (CoreML)"
        }
    );

    let db_path = catalog_path();
    let conn = open_catalog(&db_path)?;
    let mut labeled = labeled_images(&conn).context("query labeled_images")?;
    // `LIMIT=N` caps the run (first N by id) — for the CPU-vs-CoreML parity/perf subset, which would
    // otherwise pay a full CPU MD@1280 pass.
    if let Some(n) = std::env::var("LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
    {
        labeled.truncate(n);
    }
    eprintln!(
        "catalog: {}  | {} labeled images",
        db_path.display(),
        labeled.len()
    );

    let (mut person, mut animal) = (Cat::default(), Cat::default());
    let (mut ok, mut failed) = (0usize, 0usize);
    let stdout = std::io::stdout();

    for li in &labeled {
        let path = Path::new(&li.path);
        let img = match decode_srgb(path) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("skip {} (decode: {e:#})", li.path);
                failed += 1;
                continue;
            }
        };
        // People score comes from D-FINE's per-category map; Animals from MegaDetector.
        let det = match detector.score_raw(&img) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("skip {} (D-FINE: {e:#})", li.path);
                failed += 1;
                continue;
            }
        };
        let md = match mega.score_raw(&img) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("skip {} (MD: {e:#})", li.path);
                failed += 1;
                continue;
            }
        };
        let p = det.get("People").cloned().unwrap_or_default();
        let RawScore {
            best_raw: animal_raw,
            verifier_prob: animal_vprob,
            gated: animal_gated,
        } = md;

        person.push(p.best_raw, p.gated, li.person);
        animal.push(animal_raw, animal_gated, li.animal);

        let row = EvalRow {
            image_id: li.id,
            path: &li.path,
            person_label: li.person,
            animal_label: li.animal,
            person_raw: p.best_raw,
            person_vprob: p.verifier_prob,
            person_gated: p.gated,
            animal_raw,
            animal_vprob,
            animal_gated,
        };
        serde_json::to_writer(&stdout, &row).context("write row")?;
        println!();
        ok += 1;
    }

    eprintln!("\n── summary (decoded {ok}, skipped {failed}) ──");
    person.summary("person");
    animal.summary("animal");
    eprintln!(
        "(CURRENT gate = the decision today's detect() makes; raw PR-AUC = headroom from re-thresholding)"
    );
    Ok(())
}
