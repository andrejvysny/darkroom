//! Fit the pick/reject suggestion model against a real catalog and print how well it does —
//! WITHOUT touching the database. The app's own trigger stores + promotes; this is the harness for
//! judging whether a library has enough (and good enough) labels for the feature to be worth
//! showing at all.
//!
//! Usage: cargo run -p core-library --example train_suggest
//!        cargo run -p core-library --example train_suggest /path/to/catalog.db
//!        DB=/path/to/catalog.db TAG=mobileclip-s1-v1 cargo run -p core-library --example train_suggest

use std::collections::BTreeMap;
use std::path::PathBuf;

use core_db::Db;
use core_suggest::{LabelProvenance, Sample, TAU_UNREACHABLE};

type Err = Box<dyn std::error::Error>;

/// Encoder tag the stored embeddings must carry — weights fit against one encoder are meaningless
/// against another, so the universe is scoped to a single tag (mirrors `src-tauri/src/analysis.rs`).
const DEFAULT_TAG: &str = "mobileclip-s1-v1";

fn db_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    if let Ok(p) = std::env::var("DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.andrejvysny.darkroom/catalog.db")
}

fn provenance_label(p: LabelProvenance) -> &'static str {
    match p {
        LabelProvenance::Unprompted => "unprompted",
        LabelProvenance::Override => "override",
        LabelProvenance::AgreeLo => "agree-lo",
        LabelProvenance::AgreeHi => "agree-hi",
        LabelProvenance::Batch => "batch (never trained on)",
    }
}

fn describe(samples: &[Sample]) {
    let picks = samples.iter().filter(|s| s.y).count();
    let mut by_provenance: BTreeMap<&str, usize> = BTreeMap::new();
    for s in samples {
        *by_provenance
            .entry(provenance_label(s.provenance))
            .or_insert(0) += 1;
    }
    let groups: std::collections::BTreeSet<u64> = samples.iter().map(|s| s.group).collect();
    let bursts = samples
        .iter()
        .fold(BTreeMap::<u64, usize>::new(), |mut m, s| {
            *m.entry(s.group).or_insert(0) += 1;
            m
        })
        .values()
        .filter(|&&n| n > 1)
        .count();

    println!("── labels ──");
    println!("  {:<24} {}", "picks", picks);
    println!("  {:<24} {}", "rejects", samples.len() - picks);
    for (name, n) in &by_provenance {
        println!("  {name:<24} {n}");
    }
    println!(
        "  {:<24} {} ({bursts} with >1 frame)",
        "groups",
        groups.len()
    );
}

fn main() -> Result<(), Err> {
    let path = db_path();
    let tag = std::env::var("TAG").unwrap_or_else(|_| DEFAULT_TAG.to_string());
    eprintln!("db:  {}", path.display());
    eprintln!("tag: {tag}");

    // `Db::open` migrates, which is a write — acceptable here (the app would do it on next launch
    // anyway) and the only supported way in. Nothing below writes a row.
    let db = Db::open(&path)?;
    let (samples, ids) = core_library::assemble_samples(&db.conn, &tag)?;
    if samples.is_empty() {
        eprintln!(
            "no labeled images with a `{tag}` embedding — run an AI scan with the embeddings stage, \
             then flag some photos"
        );
        return Ok(());
    }
    describe(&samples);
    eprintln!("(first ids: {:?})", &ids[..ids.len().min(5)]);

    let (model, report) = core_suggest::train(&samples, 5, &[], None, &tag, 0)?;

    println!("\n── λ sweep (out-of-fold, uninfluenced labels only) ──");
    println!(
        "  {:<10} {:>7} {:>8} {:>7} {:>11} {:>7}",
        "lambda", "auc", "auprc", "tau", "tau_reject", "top1"
    );
    for r in &report.per_lambda {
        let reject = if r.tau_reject > 1.0 {
            "unreachable".to_string()
        } else {
            format!("{:.3}", r.tau_reject)
        };
        println!(
            "  {:<10.4} {:>7.3} {:>8.3} {:>7.3} {reject:>11} {:>7}",
            r.lambda,
            r.auc,
            r.auprc,
            r.tau,
            r.top1_agreement
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "-".into()),
        );
    }

    println!("\n── selected ──");
    println!("  best lambda     {:.4}", report.best_lambda);
    println!("  cv auc          {:.3}", report.cv_auc);
    println!("  cv auprc        {:.3}", report.cv_auprc);
    println!("  tau (pick)      {:.3}", model.tau);
    if model.tau_reject >= TAU_UNREACHABLE {
        println!("  tau (reject)    unreachable — no operating point met the precision floor");
    } else {
        println!("  tau (reject)    {:.3} on 1-p", model.tau_reject);
    }
    match report.top1_agreement {
        Some(v) => println!("  burst top-1     {v:.3}"),
        None => println!("  burst top-1     - (no burst had both a pick and a reject)"),
    }
    println!(
        "  trained on      {} pos / {} neg",
        model.n_pos, model.n_neg
    );
    eprintln!("\n(read-only: nothing was written to the catalog)");
    Ok(())
}
