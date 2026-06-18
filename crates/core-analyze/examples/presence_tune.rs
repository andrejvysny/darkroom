//! Threshold sweep — reads `presence_eval` JSONL on **stdin** and grid-searches the per-category
//! detector accept threshold and the (shared) CLIP verifier-accept for **max F1** against the hand
//! labels, then prints the recommended baked-in constants. Pure offline analysis; no file writes.
//!
//! Production faithfulness: the detector thresholds are independent constants (`coco::threshold` for
//! People, `MegaDetector` threshold for Animals) so each is maximized separately — but the verifier
//! accept (`VERIFY_ACCEPT`) is a SINGLE global const, so it is chosen jointly (the `va` maximizing
//! `F1_person + F1_animal`, with each category's threshold re-optimized at that `va`).
//!
//! Ablations: "verifier off" re-optimizes ignoring `*_vprob` (raw scores are verifier-independent, so
//! this needs no second run) → the F1 delta is the verifier's contribution. The MARGIN gate changes
//! which candidates exist, so its ablation needs a second eval run; pass that JSONL as an argument:
//!   DARKROOM_DET_MARGIN=1.0 cargo run -p core-analyze --example presence_eval > /tmp/p_m1.jsonl
//!   cargo run -p core-analyze --example presence_eval | \
//!     cargo run -p core-analyze --example presence_tune -- /tmp/p_m1.jsonl
//!
//! Caveat: `person_raw` is floor-gated at `DARKROOM_DET_FLOOR` (default 0.50) in `presence_eval`, so
//! sweeping the person threshold BELOW that floor is a no-op here — to explore lower person gates,
//! re-run `presence_eval` with `DARKROOM_DET_FLOOR=0.30`.

use std::io::Read;

use anyhow::{Context, Result};
use core_analyze::metrics::{pr_auc, Confusion};
use serde::Deserialize;

/// One `presence_eval` row (unknown fields like image_id/path are ignored).
#[derive(Deserialize)]
struct Row {
    person_label: Option<bool>,
    animal_label: Option<bool>,
    person_raw: f32,
    person_vprob: Option<f32>,
    person_gated: bool,
    animal_raw: f32,
    animal_vprob: Option<f32>,
    animal_gated: bool,
}

#[derive(Clone, Copy)]
struct Sample {
    raw: f32,
    vprob: Option<f32>,
    gated: bool,
    label: bool,
}

fn parse(text: &str) -> Result<Vec<Row>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Row>(l).context("parse JSONL row"))
        .collect()
}

fn person_samples(rows: &[Row]) -> Vec<Sample> {
    rows.iter()
        .filter_map(|r| {
            r.person_label.map(|label| Sample {
                raw: r.person_raw,
                vprob: r.person_vprob,
                gated: r.person_gated,
                label,
            })
        })
        .collect()
}

fn animal_samples(rows: &[Row]) -> Vec<Sample> {
    rows.iter()
        .filter_map(|r| {
            r.animal_label.map(|label| Sample {
                raw: r.animal_raw,
                vprob: r.animal_vprob,
                gated: r.animal_gated,
                label,
            })
        })
        .collect()
}

/// Confusion for predicting positive iff `raw >= th` AND the verifier accepts. `va = Some(a)` gates on
/// `vprob >= a` (None vprob = no prompt set → accept); `va = None` disables the verifier entirely.
fn conf_at(samples: &[Sample], th: f32, va: Option<f32>) -> Confusion {
    let mut c = Confusion::default();
    for s in samples {
        let vpass = match va {
            Some(a) => s.vprob.is_none_or(|p| p >= a),
            None => true,
        };
        match (s.raw >= th && vpass, s.label) {
            (true, true) => c.tp += 1,
            (true, false) => c.fp += 1,
            (false, true) => c.fn_ += 1,
            (false, false) => c.tn += 1,
        }
    }
    c
}

fn f1_of(c: &Confusion) -> f32 {
    c.f1().unwrap_or(0.0)
}

/// Best detector threshold (0.30..=0.70 step .01) for `samples` at a fixed verifier setting, by max F1
/// with precision as the tie-break. Returns `(threshold, confusion)`.
fn best_threshold(samples: &[Sample], va: Option<f32>) -> (f32, Confusion) {
    let mut best: Option<(f32, Confusion, f32, f32)> = None; // th, conf, f1, precision
    for i in 30..=70 {
        let th = i as f32 / 100.0;
        let c = conf_at(samples, th, va);
        let (f1, prec) = (f1_of(&c), c.precision().unwrap_or(0.0));
        let better = match &best {
            None => true,
            Some((_, _, bf, bp)) => f1 > bf + 1e-6 || ((f1 - bf).abs() <= 1e-6 && prec > *bp),
        };
        if better {
            best = Some((th, c, f1, prec));
        }
    }
    let (th, c, _, _) = best.expect("threshold grid is non-empty");
    (th, c)
}

fn fmt(o: Option<f32>) -> String {
    o.map(|v| format!("{v:.3}")).unwrap_or_else(|| "n/a".into())
}

fn line(tag: &str, c: &Confusion) -> String {
    format!(
        "{tag:<22} P={} R={} F1={:.3}  (tp {} fp {} fn {} tn {})",
        fmt(c.precision()),
        fmt(c.recall()),
        f1_of(c),
        c.tp,
        c.fp,
        c.fn_,
        c.tn,
    )
}

fn report(name: &str, samples: &[Sample], shared_va: f32) {
    let n_pos = samples.iter().filter(|s| s.label).count();
    let n_neg = samples.len() - n_pos;
    println!(
        "\n=== {name}  (n={}, pos {n_pos}, neg {n_neg}) ===",
        samples.len()
    );

    // Current shipped pipeline (the `gated` column).
    let mut base = Confusion::default();
    for s in samples {
        match (s.gated, s.label) {
            (true, true) => base.tp += 1,
            (true, false) => base.fp += 1,
            (false, true) => base.fn_ += 1,
            (false, false) => base.tn += 1,
        }
    }
    println!("  {}", line("current gate:", &base));

    let (th_v, c_v) = best_threshold(samples, Some(shared_va));
    println!(
        "  {}   [th={th_v:.2}, shared va={shared_va:.2}]  ΔF1 vs current {:+.3}",
        line("best w/ verifier:", &c_v),
        f1_of(&c_v) - f1_of(&base)
    );

    let (th_off, c_off) = best_threshold(samples, None);
    println!(
        "  {}   [th={th_off:.2}]  verifier ΔF1 {:+.3}",
        line("best, verifier OFF:", &c_off),
        f1_of(&c_v) - f1_of(&c_off)
    );

    let pr: Vec<(f32, bool)> = samples.iter().map(|s| (s.raw, s.label)).collect();
    println!("  raw score PR-AUC: {}", fmt(pr_auc(&pr)));
}

/// Joint shared verifier-accept (0.20..=0.60 step .05) maximizing `F1_person + F1_animal`, with each
/// category threshold re-optimized at that `va`.
fn best_shared_va(person: &[Sample], animal: &[Sample]) -> f32 {
    let mut best: Option<(f32, f32)> = None; // va, combined F1
    for j in 4..=12 {
        let va = j as f32 / 20.0;
        let (_, cp) = best_threshold(person, Some(va));
        let (_, ca) = best_threshold(animal, Some(va));
        let combined = f1_of(&cp) + f1_of(&ca);
        if best.is_none_or(|(_, b)| combined > b + 1e-6) {
            best = Some((va, combined));
        }
    }
    best.expect("va grid is non-empty").0
}

fn run(label: &str, rows: &[Row]) {
    let person = person_samples(rows);
    let animal = animal_samples(rows);
    let va = best_shared_va(&person, &animal);
    println!("\n######## {label}  (shared verifier accept = {va:.2}) ########");
    report("person", &person, va);
    report("animal", &animal, va);

    let (th_p, _) = best_threshold(&person, Some(va));
    let (th_a, _) = best_threshold(&animal, Some(va));
    println!("\n  ── recommended constants ({label}) ──");
    println!("  coco::threshold(\"People\")   = {th_p:.2}   (DARKROOM_TH_PEOPLE)");
    println!("  MegaDetector DEFAULT_THRESHOLD = {th_a:.2}  (DARKROOM_MD_THRESHOLD)");
    println!("  verify::VERIFY_ACCEPT        = {va:.2}   (DARKROOM_VERIFY_ACCEPT, shared)");
}

fn main() -> Result<()> {
    let mut stdin = String::new();
    std::io::stdin()
        .read_to_string(&mut stdin)
        .context("read stdin")?;
    let rows = parse(&stdin)?;
    if rows.is_empty() {
        anyhow::bail!("no rows on stdin — pipe `presence_eval` output in");
    }
    run("primary (verifier on, default gates)", &rows);

    // Optional comparison set (e.g. margin-relaxed eval) passed as an argument.
    if let Some(path) = std::env::args().nth(1) {
        let text = std::fs::read_to_string(&path).with_context(|| format!("read {path}"))?;
        let alt = parse(&text)?;
        if !alt.is_empty() {
            run(&format!("ALT: {path}"), &alt);
        }
    }
    Ok(())
}
