//! Evidence for (or against) `worker_count()`'s current caps (all cores for Reference, `min(cores,
//! 4)` for Copy/Move — `core-import/src/lib.rs`). Each worker holds the WHOLE source file in memory
//! (`Arc::new(std::fs::read(src_path))`) before hashing/decoding, so on a 12-core Apple Silicon
//! machine "all cores" is a dozen full RAW buffers plus a dozen decode working sets in flight at
//! once — on the 8 GB unified-memory target that can be slower than four workers, and it competes
//! with the foreground app. This measures wall time AND peak RSS across worker counts so a cap
//! change is backed by numbers, not a guess.
//!
//! Real files only — no synthetic RAW. A decode benchmark over fake bytes measures nothing (rawler
//! never gets far enough to do real work), and copying one fixture N times would just dedupe by
//! content hash. Point this at an actual card, or a folder copied from one:
//!
//!   cargo run --release -p core-import --example bench_import -- /path/to/card
//!   cargo run --release -p core-import --example bench_import -- /path/to/card 2 4 8
//!   cargo run --release -p core-import --example bench_import -- /path/to/card --repeat 3 --keep
//!
//! NEVER benchmarks Move mode — it Trashes the source after a verified copy, so running it here
//! would eat the operator's card contents for a number that's only useful in aggregate. Reference
//! (pure CPU) and Copy (CPU + destination-volume I/O) already exercise the two paths `worker_count`
//! distinguishes.

use core_db::Db;
use core_import::{import, ImportMode, Pairing};
use core_library::{enumerate_raws, ThumbCache};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The two modes worth benchmarking. Move is deliberately not a variant here — see the module doc.
#[derive(Clone, Copy)]
enum BenchMode {
    Reference,
    Copy,
}

impl BenchMode {
    fn label(self) -> &'static str {
        match self {
            BenchMode::Reference => "reference",
            BenchMode::Copy => "copy",
        }
    }

    fn import_mode(self) -> ImportMode {
        match self {
            BenchMode::Reference => ImportMode::Reference,
            BenchMode::Copy => ImportMode::Copy,
        }
    }
}

/// Whole-process peak RSS via `getrusage(RUSAGE_SELF)`. `ru_maxrss` is BYTES on macOS but
/// KILOBYTES on Linux (an old BSD/Linux `getrusage(2)` inconsistency neither side ever fixed) —
/// normalized to bytes here so callers never have to think about the unit. `None` where this isn't
/// wired up (Windows, or the syscall failing) — the table prints `n/a` and the report says so.
#[cfg(unix)]
fn peak_rss_bytes() -> Option<u64> {
    // SAFETY: `rusage` is a plain-old-data struct; zero-init + letting the kernel fill it in via
    // `getrusage` is the documented usage pattern. Single-threaded call site, no aliasing.
    let usage = unsafe {
        let mut usage: libc::rusage = std::mem::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, &mut usage) != 0 {
            return None;
        }
        usage
    };
    #[cfg(target_os = "macos")]
    {
        Some(usage.ru_maxrss as u64)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(usage.ru_maxrss as u64 * 1024)
    }
}

#[cfg(not(unix))]
fn peak_rss_bytes() -> Option<u64> {
    None // not measured on this platform — see the module doc / final report.
}

/// Installed RAM, for the report's "this isn't the 8 GB target" caveat. macOS only (`sysctl`); other
/// platforms just skip the number rather than guess at it.
fn system_ram_gb() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u64>()
            .ok()
            .map(|b| b as f64 / 1e9)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

struct RunResult {
    workers: usize,
    rep: u32,
    wall: Duration,
    total: usize,
    added: usize,
    skipped: usize,
    failed: usize,
    /// See `peak_rss_bytes`: a whole-process high-water mark, monotone non-decreasing for the life
    /// of this benchmark run — NOT this config's isolated usage. Configs run in ascending-worker
    /// order within each mode specifically so consecutive readings are at least directionally
    /// meaningful (a jump means that config really did push memory higher; a flat reading is
    /// inconclusive, not proof of "used no more").
    peak_rss_bytes: Option<u64>,
}

/// One full import into a FRESH temp destination + fresh temp catalog + fresh thumb cache, so this
/// run never sees another run's rows or copies (a Copy run reusing a catalog would see its own
/// earlier copies as already-`present` and silently turn `added` into `skipped`).
fn run_once(
    source: &Path,
    mode: ImportMode,
    workers: usize,
    rep: u32,
    keep: bool,
) -> Result<RunResult, Box<dyn std::error::Error>> {
    let dest_dir = tempfile::tempdir()?;
    let thumb_dir = tempfile::tempdir()?;
    let catalog_dir = tempfile::tempdir()?;
    let catalog_path = catalog_dir.path().join("catalog.db");

    let thumbs = ThumbCache::new(thumb_dir.path())?;
    let db = Mutex::new(Db::open(&catalog_path)?);

    // Force this run's worker count. `import` builds its rayon pool from `worker_count()`
    // synchronously on this thread before anything runs in parallel — this call happens before that,
    // on the only thread alive at this point, so the `set_var` safety requirement (no concurrent
    // reader/writer of the environment) is met.
    unsafe {
        std::env::set_var("DARKROOM_IMPORT_WORKERS", workers.to_string());
    }

    let t = Instant::now();
    let stats = import(
        &db,
        &thumbs,
        source,
        mode,
        dest_dir.path(),
        true,                // recursive: a real card nests by manufacturer subfolder
        Pairing::Standalone, // pairing's extra DB pass is orthogonal to worker-count scaling
        |_, _, _| {},
    )?;
    let wall = t.elapsed();
    let peak_rss = peak_rss_bytes();

    let sum = stats.added + stats.skipped + stats.failed;
    if sum != stats.total {
        println!(
            "  !! MISMATCH mode={mode:?} workers={workers} rep={rep}: added({}) + skipped({}) + \
             failed({}) = {sum} != total {}",
            stats.added, stats.skipped, stats.failed, stats.total
        );
    }

    if keep {
        println!(
            "  kept: dest={} thumbs={} catalog={}",
            dest_dir.keep().display(),
            thumb_dir.keep().display(),
            catalog_dir.keep().display()
        );
    }
    // else: `db` and `thumbs` (declared after the three TempDirs) drop first here, releasing the
    // sqlite connection and thumb-cache handles; the TempDirs then drop and delete themselves.

    Ok(RunResult {
        workers,
        rep,
        wall,
        total: stats.total,
        added: stats.added,
        skipped: stats.skipped,
        failed: stats.failed,
        peak_rss_bytes: peak_rss,
    })
}

fn print_table(all: &[(BenchMode, Vec<RunResult>)], repeat: u32) {
    println!(
        "{:<10} {:>7} {:>9} {:>10} {:>10}   {:>6}/{:>7}/{:>6}",
        "mode", "workers", "wall(s)", "files/min", "peakRSS", "added", "skip", "fail"
    );
    for (mode, runs) in all {
        for r in runs {
            let files_per_min = r.total as f64 / r.wall.as_secs_f64().max(f64::EPSILON) * 60.0;
            let rss = r
                .peak_rss_bytes
                .map(|b| format!("{:.0}MB", b as f64 / 1e6))
                .unwrap_or_else(|| "n/a".into());
            let rep_tag = if repeat > 1 {
                format!(" rep{}", r.rep)
            } else {
                String::new()
            };
            println!(
                "{:<10} {:>7} {:>9.1} {:>10.1} {:>10}   {:>6}/{:>7}/{:>6}{rep_tag}",
                mode.label(),
                r.workers,
                r.wall.as_secs_f64(),
                files_per_min,
                rss,
                r.added,
                r.skipped,
                r.failed,
            );
        }
    }
}

/// One recommendation line per mode. NOT memory-ranked across configs — peak RSS readings are a
/// shared monotone ceiling (see `RunResult::peak_rss_bytes`), so comparing them config-to-config is
/// unsound. "Best time-to-memory tradeoff" is instead the SMALLEST worker count that still reaches
/// ~90% of the fastest throughput (a diminishing-returns pick) — genuinely comparable, because wall
/// time IS measured fresh and correctly for every run. Its RSS reading is reported alongside for
/// context only, not as the ranking criterion.
fn recommend(mode: BenchMode, runs: &[RunResult]) {
    let mut by_workers: BTreeMap<usize, Vec<&RunResult>> = BTreeMap::new();
    for r in runs {
        by_workers.entry(r.workers).or_default().push(r);
    }

    let stat = |v: &[&RunResult]| -> (f64, Option<f64>) {
        let fpm = v
            .iter()
            .map(|r| r.total as f64 / r.wall.as_secs_f64().max(f64::EPSILON) * 60.0)
            .sum::<f64>()
            / v.len() as f64;
        let rss_mb = v
            .iter()
            .filter_map(|r| r.peak_rss_bytes)
            .max()
            .map(|b| b as f64 / 1e6);
        (fpm, rss_mb)
    };

    let ranked: Vec<(usize, f64, Option<f64>)> = by_workers
        .iter()
        .map(|(&w, v)| {
            let (fpm, rss) = stat(v);
            (w, fpm, rss)
        })
        .collect();

    let Some(fastest) = ranked.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1)) else {
        return;
    };
    let tradeoff = ranked
        .iter()
        .copied()
        .find(|&(_, fpm, _)| fpm >= fastest.1 * 0.9)
        .unwrap_or(fastest);

    let fmt_mb = |v: Option<f64>| {
        v.map(|x| format!("{x:.0} MB"))
            .unwrap_or_else(|| "n/a".into())
    };
    println!(
        "[{}] fastest: workers={} ({:.1} files/min, peak RSS ceiling {}) | best time-to-memory: \
         workers={} ({:.1} files/min, {:.0}% of fastest, peak RSS ceiling {}) — this machine is NOT \
         the 8 GB target; use as a starting point, not a verdict.",
        mode.label(),
        fastest.0,
        fastest.1,
        fmt_mb(fastest.2),
        tradeoff.0,
        tradeoff.1,
        tradeoff.1 / fastest.1 * 100.0,
        fmt_mb(tradeoff.2),
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut positional: Vec<String> = Vec::new();
    let mut repeat: u32 = 1;
    let mut keep = false;
    let mut raw_args = std::env::args().skip(1);
    while let Some(a) = raw_args.next() {
        match a.as_str() {
            "--keep" => keep = true,
            "--repeat" => {
                let n = raw_args.next().ok_or("--repeat requires a value")?;
                repeat = n
                    .parse()
                    .map_err(|_| format!("'{n}' is not a valid --repeat count"))?;
            }
            other => positional.push(other.to_string()),
        }
    }

    if positional.is_empty() {
        println!(
            "usage: bench_import <source-dir> [workers...] [--repeat N] [--keep]\n\
             point <source-dir> at a real folder of camera files (a mounted card, or a copy of one) \
             — this measures real decode/copy/hash work over real RAWs, so no directory means \
             nothing to benchmark."
        );
        return Ok(());
    }

    let source = PathBuf::from(&positional[0]);
    if !source.is_dir() {
        return Err(format!("{} is not a directory", source.display()).into());
    }

    // Real files only (see module doc) — this is also the honest "is there anything here" check,
    // using the same gate `import` itself uses (`core_library::SUPPORTED_EXT`).
    let files = enumerate_raws(&source, true);
    if files.is_empty() {
        println!(
            "no files under {} match core_library::SUPPORTED_EXT — point this at a real card/folder \
             of camera files, not an empty or unsupported-format directory.",
            source.display()
        );
        return Ok(());
    }

    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut workers: Vec<usize> = if positional.len() > 1 {
        positional[1..]
            .iter()
            .map(|s| {
                s.parse::<usize>()
                    .map_err(|_| format!("'{s}' is not a valid worker count"))
            })
            .collect::<Result<_, _>>()?
    } else {
        vec![1, 2, 3, 4, 6, 8, cores]
    };
    workers.retain(|&w| w > 0);
    workers.sort_unstable();
    workers.dedup();
    if workers.is_empty() {
        return Err("no valid worker counts given".into());
    }

    println!("source: {} ({} real files)", source.display(), files.len());
    println!(
        "workers: {workers:?}  repeat: {repeat}  keep: {keep}  cores: {cores}{}",
        system_ram_gb()
            .map(|gb| format!("  ram: ~{gb:.0} GB"))
            .unwrap_or_default()
    );
    println!(
        "NOTE peak RSS = `ru_maxrss` sampled once per run — a whole-process HIGH-WATER MARK, \
         monotone non-decreasing for this whole benchmark process. It is the ceiling reached BY the \
         time a config finished, not that config's isolated usage; only a genuine jump between \
         consecutive (ascending-worker) rows within a mode is real signal, a flat reading is \
         inconclusive."
    );
    println!(
        "NOTE Move mode is never benchmarked — it Trashes the source after a verified copy; \
         Reference (pure CPU) and Copy (CPU + destination I/O) already cover the two paths \
         worker_count() distinguishes.\n"
    );

    let mut all: Vec<(BenchMode, Vec<RunResult>)> = Vec::new();
    for mode in [BenchMode::Reference, BenchMode::Copy] {
        let mut runs = Vec::new();
        for &w in &workers {
            for rep in 1..=repeat {
                print!("running {} workers={w} rep={rep}/{repeat} … ", mode.label());
                std::io::stdout().flush().ok();
                match run_once(&source, mode.import_mode(), w, rep, keep) {
                    Ok(r) => {
                        println!(
                            "{:.1}s  {}/{}/{} added/skipped/failed",
                            r.wall.as_secs_f64(),
                            r.added,
                            r.skipped,
                            r.failed
                        );
                        runs.push(r);
                    }
                    Err(e) => println!("FAILED: {e}"),
                }
            }
        }
        all.push((mode, runs));
    }

    println!();
    print_table(&all, repeat);
    println!();
    for (mode, runs) in &all {
        if !runs.is_empty() {
            recommend(*mode, runs);
        }
    }

    Ok(())
}
