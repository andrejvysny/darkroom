//! No-GUI harness measuring the Library query surface at synthetic scale, so we know whether
//! 100k images is fine (and, if not, WHICH query falls over) before touching any SQL. The scale
//! migration (`core-db/migrations/003_scale.sql`) was tuned for 10k-50k; this is the falsifier.
//!
//! Usage: cargo run --release -p core-library --example bench_catalog -- [SIZE..] [--keep]
//!   (no SIZE args -> 10000 50000 100000; --keep leaves the generated catalog.db on disk)
//!
//! Rows only — no image files, no thumbnails, no rawler/decoder calls. A temp on-disk SQLite file
//! per size (never `:memory:`): page-cache behavior under WAL is part of what we're measuring, and
//! an in-memory DB would hide exactly the I/O cost a real multi-GB catalog pays.
//!
//! Deliberately runs NO `ANALYZE` after generating: `core_db::Db::open`'s pragma set doesn't run it
//! either, so a real installed catalog never gets one. Matching that means the query planner here
//! sees exactly what a user's planner sees — not a best-case, stats-informed one.
//!
//! ## Reconstructed SQL (read this before trusting a plan)
//! `query.rs` keeps its `WHERE`/`COLUMNS`/join builders `crate`-private (not `pub`), so the
//! `EXPLAIN QUERY PLAN` calls below re-literal them by hand — the `RECON_*` constants and
//! `recon_*`/`explain_*` functions are byte-for-byte copies of `query.rs` as of 2026-09-09. Every
//! **timing** number always goes through the real public functions (`query_images`, `count_images`,
//! `list_folders`, `date_tree`, `list_keywords`, `list_collections`) — only the **plans** printed
//! under them are a reconstruction, and only those can go stale if `query.rs`'s filter set changes.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use core_db::rusqlite::{params, Connection, ToSql};
use core_db::Db;
use core_library::{
    count_images, date_tree, list_collections, list_folders, list_keywords, query_images, LibError,
    QueryParams,
};

type Err = Box<dyn std::error::Error>;

const DEFAULT_SIZES: [usize; 3] = [10_000, 50_000, 100_000];
const RUNS: usize = 5;
const PAGE: i64 = 500;

// Fixture shape knobs. Kept fixed across sizes (not scaled) on purpose: a "few hundred folders" /
// "~40 keywords" library stays that size as it grows, so this keeps 10k vs 100k comparable instead
// of confounding "more rows" with "more folders."
const NUM_FOLDERS: usize = 300;
const NUM_KEYWORDS: usize = 40;
const NUM_COLLECTIONS: usize = 8; // last one is the lone smart collection

// Fixed anchor instead of `SystemTime::now()` so the fixture (and its printed day/year) is
// reproducible byte-for-byte between runs, independent of wall-clock date.
const ANCHOR_EPOCH: i64 = 1_700_000_000; // 2023-11-14
const SPAN_SECS: i64 = 4 * 365 * 86_400; // ~4 years of capture-date spread

fn main() -> Result<(), Err> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let keep = if let Some(pos) = args.iter().position(|a| a == "--keep") {
        args.remove(pos);
        true
    } else {
        false
    };
    let sizes: Vec<usize> = if args.is_empty() {
        DEFAULT_SIZES.to_vec()
    } else {
        args.iter()
            .map(|a| a.parse().expect("SIZE must be a positive integer"))
            .collect()
    };

    for size in sizes {
        // A measurement tool, not a test: one size erroring out must not stop the others, and the
        // process must still exit 0 (requirement — someone is scripting `for size in ...` around
        // this, not gating CI on it).
        if let Err(e) = run_bench(size, keep) {
            eprintln!("size {size}: FAILED: {e}");
        }
    }
    Ok(())
}

fn run_bench(size: usize, keep: bool) -> Result<(), Err> {
    println!("\n========== size = {size} ==========");
    let path = std::env::temp_dir().join(format!("darkroom-bench-catalog-{size}.db"));
    for ext in ["db", "db-wal", "db-shm"] {
        let _ = std::fs::remove_file(path.with_extension(ext));
    }

    let mut db = Db::open(&path)?;
    let t = Instant::now();
    let fixture = generate(&mut db.conn, size)?;
    // Steady-state size, not "mid-WAL" size: a real catalog eventually checkpoints too, and we want
    // a number that means something rather than an artifact of how big the WAL grew.
    db.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let gen_dt = t.elapsed();
    let db_bytes = file_size(&path) + file_size(&path.with_extension("db-wal"));
    println!(
        "generated {size} rows in {:.2?}  ({:.1} MB on disk)",
        gen_dt,
        db_bytes as f64 / 1_048_576.0
    );

    let mut table: Vec<Row> = Vec::new();
    run_queries(&db.conn, &fixture, &mut table)?;

    print_table(&table);
    print_over_budget(&table);

    drop(db);
    if keep {
        println!("kept: {}", path.display());
    } else {
        for ext in ["db", "db-wal", "db-shm"] {
            let _ = std::fs::remove_file(path.with_extension(ext));
        }
    }
    Ok(())
}

fn file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// ================================================================================================
// Deterministic RNG — xorshift64* keyed off `size` so each size gets an independent-looking but
// 100%-reproducible fixture. No `rand` dependency: this file adds none, per the task brief.
// ================================================================================================

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1) // xorshift needs non-zero state
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64) // [0, 1)
    }
    /// Uniform integer in `[lo, hi)`.
    fn gen_range(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(hi > lo);
        lo + (self.next_u64() % (hi - lo) as u64) as i64
    }
}

/// Sample an index from an ascending cumulative-probability table (Zipf keyword ranks).
fn weighted_index(rng: &mut Rng, cum: &[f64]) -> usize {
    let r = rng.next_f64();
    cum.iter().position(|&c| r <= c).unwrap_or(cum.len() - 1)
}

/// Days-since-epoch (1970-01-01) -> proleptic-Gregorian (year, month, day). Pure-integer port of
/// Howard Hinnant's `civil_from_days`, used only so `capture_year`/`capture_date` filter params
/// (formatted here) agree with SQLite's `strftime('...','unixepoch')` (also proleptic Gregorian) —
/// cheaper than pulling in a date crate for two format strings this file must not depend on.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn day_string(epoch: i64) -> String {
    let (y, m, d) = civil_from_days(epoch.div_euclid(86_400));
    format!("{y:04}-{m:02}-{d:02}")
}

fn year_string(epoch: i64) -> String {
    let (y, _, _) = civil_from_days(epoch.div_euclid(86_400));
    format!("{y:04}")
}

fn pick_ext(rng: &mut Rng) -> (&'static str, &'static str) {
    // Weighted toward RAW (this app's whole point), with a JPEG minority — matches the
    // `SUPPORTED_EXT` bucket split `core_raw::classify` actually assigns.
    let r = rng.next_f64();
    if r < 0.42 {
        ("cr3", "raw")
    } else if r < 0.56 {
        ("nef", "raw")
    } else if r < 0.68 {
        ("arw", "raw")
    } else if r < 0.75 {
        ("dng", "raw")
    } else {
        ("jpg", "jpeg")
    }
}

fn pick_camera(rng: &mut Rng) -> &'static str {
    let r = rng.next_f64();
    if r < 0.55 {
        "Canon EOS R7"
    } else if r < 0.70 {
        "Canon EOS R6"
    } else if r < 0.82 {
        "Nikon Z6"
    } else if r < 0.92 {
        "Sony A7C"
    } else {
        "Canon EOS R5"
    }
}

fn pick_lens(rng: &mut Rng) -> &'static str {
    const LENSES: [&str; 5] = [
        "RF 24-70mm f/2.8L",
        "RF 100-500mm f/4.5-7.1L",
        "RF 50mm f/1.8",
        "RF-S 18-150mm f/3.5-6.3",
        "RF 35mm f/1.8 Macro",
    ];
    LENSES[rng.gen_range(0, LENSES.len() as i64) as usize]
}

fn pick_prefix(rng: &mut Rng) -> &'static str {
    let r = rng.next_f64();
    if r < 0.70 {
        "IMG"
    } else if r < 0.90 {
        "DSC"
    } else {
        "_MG"
    }
}

// ================================================================================================
// Fixture generation
// ================================================================================================

/// Handles into the generated catalog the query benchmarks need — computed during generation
/// rather than guessed, so every filter/search test is guaranteed a real (non-empty, or
/// deliberately empty) match set regardless of `size` or the RNG seed.
struct Fixture {
    busiest_folder_id: i64,
    busiest_day: String,
    busiest_year: String,
    top_keyword_id: i64,
    /// Filename substring that exists on exactly one row (an explicit marker image).
    unique_token: String,
    /// Filename substring most rows contain (the dominant `pick_prefix` choice) — a "common
    /// substring" search has to LIKE-scan the whole present set, which is the point of the test.
    common_token: String,
    /// A substring that appears nowhere in the fixture's vocabulary.
    no_result_token: String,
}

#[allow(clippy::too_many_lines)]
fn generate(conn: &mut Connection, size: usize) -> Result<Fixture, Err> {
    let mut rng = Rng::new(0xD00D_D00D_u64 ^ size as u64);
    let tx = conn.transaction()?;

    // ---- folders ---------------------------------------------------------------------------
    let mut folder_ids = Vec::with_capacity(NUM_FOLDERS);
    {
        let mut stmt = tx.prepare("INSERT INTO folders(path, added_at) VALUES (?1, 0)")?;
        for i in 0..NUM_FOLDERS {
            stmt.execute(params![format!("/synthetic/folder-{i:04}")])?;
            folder_ids.push(tx.last_insert_rowid());
        }
    }

    // ---- keywords (Zipf-ish popularity) -----------------------------------------------------
    let mut keyword_ids = Vec::with_capacity(NUM_KEYWORDS);
    {
        let mut stmt = tx.prepare("INSERT INTO keywords(name) VALUES (?1)")?;
        for i in 0..NUM_KEYWORDS {
            stmt.execute(params![format!("kw-{i:02}")])?;
            keyword_ids.push(tx.last_insert_rowid());
        }
    }
    // rank 1 (index 0) is the most popular keyword by construction -> `top_keyword_id`.
    let kw_weights: Vec<f64> = (1..=NUM_KEYWORDS).map(|r| 1.0 / r as f64).collect();
    let kw_total: f64 = kw_weights.iter().sum();
    let mut kw_cum = Vec::with_capacity(NUM_KEYWORDS);
    let mut acc = 0.0;
    for w in &kw_weights {
        acc += w / kw_total;
        kw_cum.push(acc);
    }

    // ---- collections: NUM_COLLECTIONS-1 static, one smart (query is a `QueryParams` predicate)
    let mut collection_ids = Vec::with_capacity(NUM_COLLECTIONS);
    {
        let mut stmt =
            tx.prepare("INSERT INTO collections(name, is_smart, query) VALUES (?1, ?2, ?3)")?;
        for i in 0..NUM_COLLECTIONS {
            let is_smart = i == NUM_COLLECTIONS - 1;
            let query = if is_smart {
                Some(r#"{"minStars":4}"#)
            } else {
                None
            };
            stmt.execute(params![format!("Collection {i}"), is_smart, query])?;
            collection_ids.push(tx.last_insert_rowid());
        }
    }
    let static_collection_ids = &collection_ids[..NUM_COLLECTIONS - 1];

    // ---- images, clustered into "shoots" rather than spread uniformly ----------------------
    let mut folder_counts: HashMap<i64, i64> = HashMap::new();
    let mut day_counts: HashMap<String, i64> = HashMap::new();
    let mut year_counts: HashMap<String, i64> = HashMap::new();
    const COLORS: [&str; 5] = ["red", "yellow", "green", "blue", "purple"];
    const EDIT_PARAMS_JSON: &str = r#"{"exposure":0.3,"contrast":10,"highlights":-20,"shadows":15,"whites":5,"blacks":-5,"temp":5500,"tint":2,"vibrance":8,"saturation":0}"#;

    {
        let mut img_stmt = tx.prepare(
            "INSERT INTO images(
                content_hash, file_size, path, folder_id, original_filename, status, capture_date,
                camera_make, camera_model, lens, iso, shutter, aperture, focal_length, width,
                height, orientation, imported_at, format
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        )?;
        let mut rf_stmt = tx.prepare(
            "INSERT INTO ratings_flags(image_id, stars, flag, color_label) VALUES (?1,?2,?3,?4)",
        )?;
        let mut kw_stmt =
            tx.prepare("INSERT INTO image_keywords(image_id, keyword_id) VALUES (?1,?2)")?;
        let mut coll_stmt =
            tx.prepare("INSERT INTO collection_images(collection_id, image_id) VALUES (?1,?2)")?;
        let mut edit_stmt = tx.prepare(
            "INSERT INTO edits(image_id, process_version, params, updated_at) VALUES (?1,1,?2,?3)",
        )?;

        // reserve row 1 of `size` for the search-marker image inserted after the shoot loop.
        let mut remaining = size - 1;
        let mut global_idx = 0i64;

        while remaining > 0 {
            let base_size = rng.gen_range(5, 60);
            let is_burst = rng.next_f64() < 0.15;
            let shoot_size = if is_burst {
                base_size + rng.gen_range(0, 240)
            } else {
                base_size
            }
            .min(remaining as i64) as usize;

            let folder_idx = rng.gen_range(0, NUM_FOLDERS as i64) as usize;
            let folder_id = folder_ids[folder_idx];
            let shoot_base = ANCHOR_EPOCH - rng.gen_range(0, SPAN_SECS);
            let gap = rng.gen_range(2, 180); // seconds between frames in this shoot
            let (ext, format_bucket) = pick_ext(&mut rng);
            let camera = pick_camera(&mut rng);
            let camera_make = if camera.starts_with("Canon") {
                "Canon"
            } else if camera.starts_with("Nikon") {
                "Nikon"
            } else {
                "Sony"
            };
            let lens = pick_lens(&mut rng);
            let prefix = pick_prefix(&mut rng);

            for i in 0..shoot_size {
                global_idx += 1;
                // A thin slice of files without EXIF capture-date (unreadable/stripped metadata) —
                // exercises query.rs's NULL-block seek phase, not just the common path.
                let capture_date = if rng.next_f64() < 0.005 {
                    None
                } else {
                    Some(shoot_base + i as i64 * gap)
                };
                let status = if rng.next_f64() < 0.02 {
                    "missing"
                } else {
                    "present"
                };
                let filename = format!("{prefix}_{global_idx:06}.{}", ext.to_ascii_uppercase());
                let path = format!("/synthetic/folder-{folder_idx:04}/{filename}");
                let mut hash = [0u8; 32];
                hash[0..8].copy_from_slice(&(global_idx as u64).to_le_bytes());
                hash[8..16].copy_from_slice(&rng.next_u64().to_le_bytes());
                let iso = 100 * (1 + rng.gen_range(0, 32));
                let aperture = 1.4 + (rng.gen_range(0, 60) as f64) * 0.1;
                let focal_length = 24.0 + rng.gen_range(0, 180) as f64;
                let shutter = format!("1/{}", 30 + rng.gen_range(0, 4000));
                let imported_at =
                    capture_date.unwrap_or(ANCHOR_EPOCH) + rng.gen_range(0, 30 * 86_400);

                img_stmt.execute(params![
                    &hash[..],
                    1_000_000i64, // file_size: irrelevant to every query under test
                    path,
                    folder_id,
                    filename,
                    status,
                    capture_date,
                    camera_make,
                    camera,
                    lens,
                    iso,
                    shutter,
                    aperture,
                    focal_length,
                    6960i64,
                    4640i64,
                    1i64,
                    imported_at,
                    format_bucket,
                ])?;
                let image_id = tx.last_insert_rowid();

                if status == "present" {
                    *folder_counts.entry(folder_id).or_insert(0) += 1;
                    if let Some(cd) = capture_date {
                        *day_counts.entry(day_string(cd)).or_insert(0) += 1;
                        *year_counts.entry(year_string(cd)).or_insert(0) += 1;
                    }
                }

                // ~30% rated, ~15% picked / ~8% rejected (mutually exclusive), ~10% colour-labelled.
                // These three axes are independent, and (matching `cull::set_rating/set_flag/
                // set_label`) an untouched image gets NO `ratings_flags` row at all.
                let stars = if rng.next_f64() < 0.30 {
                    rng.gen_range(1, 6)
                } else {
                    0
                };
                let flag_roll = rng.next_f64();
                let flag = if flag_roll < 0.15 {
                    "pick"
                } else if flag_roll < 0.23 {
                    "reject"
                } else {
                    "none"
                };
                let color_label = if rng.next_f64() < 0.10 {
                    Some(COLORS[rng.gen_range(0, COLORS.len() as i64) as usize])
                } else {
                    None
                };
                if stars > 0 || flag != "none" || color_label.is_some() {
                    rf_stmt.execute(params![image_id, stars, flag, color_label])?;
                }

                // Zipf-ish keyword assignment, 0-4 keywords per image.
                let n_kw = {
                    let r = rng.next_f64();
                    if r < 0.30 {
                        0
                    } else if r < 0.65 {
                        1
                    } else if r < 0.85 {
                        2
                    } else if r < 0.95 {
                        3
                    } else {
                        4
                    }
                };
                let mut used_kw: Vec<i64> = Vec::with_capacity(n_kw);
                for _ in 0..n_kw {
                    for _try in 0..10 {
                        let kid = keyword_ids[weighted_index(&mut rng, &kw_cum)];
                        if !used_kw.contains(&kid) {
                            used_kw.push(kid);
                            kw_stmt.execute(params![image_id, kid])?;
                            break;
                        }
                    }
                }

                // A handful of collections; ~3% membership per static collection per image.
                for &cid in static_collection_ids {
                    if rng.next_f64() < 0.03 {
                        coll_stmt.execute(params![cid, image_id])?;
                    }
                }

                // ~20% carry a stored develop edit.
                if rng.next_f64() < 0.20 {
                    edit_stmt.execute(params![image_id, EDIT_PARAMS_JSON, imported_at])?;
                }
            }
            remaining -= shoot_size;
        }

        // Search fixture: exactly one row anywhere in the catalog contains `unique_token`.
        let marker_filename = "ZZQMARKERZZ_0000001.CR3";
        let mut marker_hash = [0u8; 32];
        marker_hash[0] = 0xEE;
        img_stmt.execute(params![
            &marker_hash[..],
            1_000_000i64,
            format!("/synthetic/folder-0000/{marker_filename}"),
            folder_ids[0],
            marker_filename,
            "present",
            Some(ANCHOR_EPOCH),
            "Canon",
            "Canon EOS R7",
            pick_lens(&mut rng),
            400i64,
            "1/500",
            2.8f64,
            50.0f64,
            6960i64,
            4640i64,
            1i64,
            ANCHOR_EPOCH,
            "raw",
        ])?;
        *folder_counts.entry(folder_ids[0]).or_insert(0) += 1;
        *day_counts.entry(day_string(ANCHOR_EPOCH)).or_insert(0) += 1;
        *year_counts.entry(year_string(ANCHOR_EPOCH)).or_insert(0) += 1;
    } // prepared statements dropped here, before commit

    tx.commit()?;

    let busiest_folder_id = *folder_counts
        .iter()
        .max_by_key(|(_, &c)| c)
        .map(|(id, _)| id)
        .expect("at least one folder has a present image");
    let busiest_day = day_counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(d, _)| d)
        .expect("at least one present image has a capture date");
    let busiest_year = year_counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(y, _)| y)
        .expect("at least one present image has a capture date");

    Ok(Fixture {
        busiest_folder_id,
        busiest_day,
        busiest_year,
        top_keyword_id: keyword_ids[0],
        unique_token: "ZZQMARKERZZ".to_string(),
        common_token: "IMG_".to_string(),
        no_result_token: "QQQNoSuchSubstringQQQ".to_string(),
    })
}

// ================================================================================================
// Timing
// ================================================================================================

struct Row {
    category: &'static str,
    name: String,
    min_ms: f64,
    median_ms: f64,
}

/// Runs `f` `RUNS` times and reports min/median wall time — a single sample is not a "hosted
/// runner"-proof measurement (first-run page-cache/VFS warmup skews it), min/median over several
/// is the cheapest thing that is.
fn measure<T, F>(mut f: F) -> Result<(f64, f64, T), LibError>
where
    F: FnMut() -> Result<T, LibError>,
{
    let mut times = Vec::with_capacity(RUNS);
    let mut last = None;
    for _ in 0..RUNS {
        let t = Instant::now();
        let r = f()?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
        last = Some(r);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = times[0];
    let median = times[times.len() / 2];
    Ok((min, median, last.expect("RUNS > 0")))
}

fn bench_query(
    conn: &Connection,
    table: &mut Vec<Row>,
    category: &'static str,
    name: &str,
    p: &QueryParams,
) -> Result<(), Err> {
    let (min, median, rows) = measure(|| query_images(conn, p))?;
    println!(
        "  [{category:<10}] {name:<28} min={min:8.2}ms  median={median:8.2}ms  rows={}",
        rows.len()
    );
    for line in explain_query(conn, p)? {
        println!("        plan: {line}");
    }
    table.push(Row {
        category,
        name: name.to_string(),
        min_ms: min,
        median_ms: median,
    });
    Ok(())
}

fn run_queries(conn: &Connection, fx: &Fixture, table: &mut Vec<Row>) -> Result<(), Err> {
    println!("\n-- first page (500 rows) --");
    let base = |sort: &str, seek: bool| QueryParams {
        sort: Some(sort.to_string()),
        limit: Some(PAGE),
        seek: if seek { Some(true) } else { None },
        ..Default::default()
    };
    bench_query(
        conn,
        table,
        "first-page",
        "capture_desc offset",
        &base("capture_desc", false),
    )?;
    bench_query(
        conn,
        table,
        "first-page",
        "capture_asc offset",
        &base("capture_asc", false),
    )?;
    bench_query(
        conn,
        table,
        "first-page",
        "capture_desc seek",
        &base("capture_desc", true),
    )?;
    bench_query(
        conn,
        table,
        "first-page",
        "capture_asc seek",
        &base("capture_asc", true),
    )?;

    println!("\n-- deep page (near the end of the present set) --");
    let n_present = count_images(conn, &QueryParams::default())?;
    let deep_offset = (n_present - PAGE).max(0);

    bench_query(
        conn,
        table,
        "deep-page",
        "filename offset(deep)",
        &QueryParams {
            sort: Some("filename".to_string()),
            limit: Some(PAGE),
            offset: Some(deep_offset),
            ..Default::default()
        },
    )?;
    bench_query(
        conn,
        table,
        "deep-page",
        "rating_desc offset(deep)",
        &QueryParams {
            sort: Some("rating_desc".to_string()),
            limit: Some(PAGE),
            offset: Some(deep_offset),
            ..Default::default()
        },
    )?;

    let (cv_capture, ci_capture) = fetch_cursor(conn, "capture_date", deep_offset)?;
    bench_query(
        conn,
        table,
        "deep-page",
        "capture_desc seek(deep)",
        &QueryParams {
            sort: Some("capture_desc".to_string()),
            limit: Some(PAGE),
            seek: Some(true),
            cursor_value: cv_capture,
            cursor_id: Some(ci_capture),
            ..Default::default()
        },
    )?;
    let (cv_imported, ci_imported) = fetch_cursor(conn, "imported_at", deep_offset)?;
    bench_query(
        conn,
        table,
        "deep-page",
        "imported_desc seek(deep)",
        &QueryParams {
            sort: Some("imported_desc".to_string()),
            limit: Some(PAGE),
            seek: Some(true),
            cursor_value: cv_imported,
            cursor_id: Some(ci_imported),
            ..Default::default()
        },
    )?;

    println!("\n-- filters --");
    let filt = |p: QueryParams| QueryParams {
        limit: Some(PAGE),
        sort: Some("capture_desc".to_string()),
        ..p
    };
    bench_query(
        conn,
        table,
        "filter",
        "folder",
        &filt(QueryParams {
            folder_id: Some(fx.busiest_folder_id),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "stars>=3",
        &filt(QueryParams {
            min_stars: Some(3),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "rejected",
        &filt(QueryParams {
            flag: Some("reject".to_string()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "color=red",
        &filt(QueryParams {
            color_label: Some("red".to_string()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "keyword(top)",
        &filt(QueryParams {
            keyword_id: Some(fx.top_keyword_id),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "capture_date(busiest)",
        &filt(QueryParams {
            capture_date: Some(fx.busiest_day.clone()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "capture_year(busiest)",
        &filt(QueryParams {
            capture_year: Some(fx.busiest_year.clone()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "format=raw",
        &filt(QueryParams {
            format: Some("raw".to_string()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "folder+stars>=3",
        &filt(QueryParams {
            folder_id: Some(fx.busiest_folder_id),
            min_stars: Some(3),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "filter",
        "keyword+year",
        &filt(QueryParams {
            keyword_id: Some(fx.top_keyword_id),
            capture_year: Some(fx.busiest_year.clone()),
            ..Default::default()
        }),
    )?;

    println!("\n-- search --");
    bench_query(
        conn,
        table,
        "search",
        "no-match",
        &filt(QueryParams {
            search: Some(fx.no_result_token.clone()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "search",
        "one-match",
        &filt(QueryParams {
            search: Some(fx.unique_token.clone()),
            ..Default::default()
        }),
    )?;
    bench_query(
        conn,
        table,
        "search",
        "common-substr",
        &filt(QueryParams {
            search: Some(fx.common_token.clone()),
            ..Default::default()
        }),
    )?;

    println!("\n-- sidebar aggregates --");
    {
        let (min, median, n) = measure(|| count_images(conn, &QueryParams::default()))?;
        println!(
            "  [sidebar   ] {:<28} min={min:8.2}ms  median={median:8.2}ms  n={n}",
            "count_images"
        );
        for line in explain_count(conn, &QueryParams::default())? {
            println!("        plan: {line}");
        }
        table.push(Row {
            category: "sidebar",
            name: "count_images".to_string(),
            min_ms: min,
            median_ms: median,
        });
    }
    {
        let (min, median, rows) = measure(|| list_folders(conn))?;
        println!(
            "  [sidebar   ] {:<28} min={min:8.2}ms  median={median:8.2}ms  rows={}",
            "list_folders",
            rows.len()
        );
        for line in run_explain(conn, RECON_LIST_FOLDERS_SQL, NO_BINDS)? {
            println!("        plan: {line}");
        }
        table.push(Row {
            category: "sidebar",
            name: "list_folders".to_string(),
            min_ms: min,
            median_ms: median,
        });
    }
    {
        let (min, median, years) = measure(|| date_tree(conn))?;
        println!(
            "  [sidebar   ] {:<28} min={min:8.2}ms  median={median:8.2}ms  years={}",
            "date_tree",
            years.len()
        );
        for line in run_explain(conn, RECON_DATE_TREE_SQL, NO_BINDS)? {
            println!("        plan: {line}");
        }
        table.push(Row {
            category: "sidebar",
            name: "date_tree".to_string(),
            min_ms: min,
            median_ms: median,
        });
    }
    {
        let (min, median, rows) = measure(|| list_keywords(conn))?;
        println!(
            "  [sidebar   ] {:<28} min={min:8.2}ms  median={median:8.2}ms  rows={}",
            "list_keywords",
            rows.len()
        );
        for line in run_explain(conn, RECON_LIST_KEYWORDS_SQL, NO_BINDS)? {
            println!("        plan: {line}");
        }
        table.push(Row {
            category: "sidebar",
            name: "list_keywords".to_string(),
            min_ms: min,
            median_ms: median,
        });
    }
    {
        let (min, median, rows) = measure(|| list_collections(conn))?;
        println!(
            "  [sidebar   ] {:<28} min={min:8.2}ms  median={median:8.2}ms  rows={}",
            "list_collections",
            rows.len()
        );
        for line in run_explain(conn, RECON_LIST_COLLECTIONS_SQL, NO_BINDS)? {
            println!("        plan: {line}");
        }
        println!(
            "        note: the smart collection re-runs count_images live — see its plan above"
        );
        table.push(Row {
            category: "sidebar",
            name: "list_collections".to_string(),
            min_ms: min,
            median_ms: median,
        });
    }

    Ok(())
}

/// The row `order_col` DESC currently holds at `offset` — used as a keyset cursor for a "deep seek"
/// test. `order_col` is one of two hardcoded column names (never user input), so interpolating it
/// is safe.
fn fetch_cursor(
    conn: &Connection,
    order_col: &str,
    offset: i64,
) -> Result<(Option<i64>, i64), Err> {
    let sql = format!(
        "SELECT {order_col}, id FROM images WHERE status='present' \
         ORDER BY {order_col} DESC, id DESC LIMIT 1 OFFSET ?1"
    );
    let row = conn.query_row(&sql, params![offset], |r| {
        Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
    })?;
    Ok(row)
}

// ================================================================================================
// Reconstructed SQL (see module doc) — kept in sync by eye with `crates/core-library/src/query.rs`
// and `keywords.rs`/`collections.rs`. Byte-for-byte copies where the source is `pub`-reachable
// text; only used to feed `EXPLAIN QUERY PLAN`, never to decide what a real query returns.
// ================================================================================================

const RECON_COLUMNS: &str = r"i.id, i.content_hash, i.path, i.original_filename, i.capture_date,
    i.camera_make, i.camera_model, i.lens, i.iso, i.shutter, i.aperture, i.focal_length,
    i.width, i.height, i.orientation,
    COALESCE(rf.stars,0), COALESCE(rf.flag,'none'), rf.color_label, e.updated_at, i.imported_at,
    i.format,
    (SELECT COUNT(*) FROM image_pairs ip WHERE ip.primary_image_id = i.id),
    (SELECT ip.primary_image_id FROM image_pairs ip WHERE ip.secondary_image_id = i.id),
    s.suggested, s.score";

const RECON_EDIT_JOIN: &str = "LEFT JOIN edits e ON e.image_id = i.id";
const RECON_SUGGEST_JOIN: &str = "LEFT JOIN image_suggestion s
         ON s.image_id = i.id AND s.withheld = 0 AND s.suggested <> 'none'";

const RECON_WHERE: &str = r"i.status = 'present'
    AND (:folder_id IS NULL OR i.folder_id = :folder_id)
    AND (:min_stars IS NULL OR COALESCE(rf.stars,0) >= :min_stars)
    AND (:flag IS NULL OR COALESCE(rf.flag,'none') = :flag)
    AND (:color_label IS NULL
         OR (:color_label = '__none__' AND rf.color_label IS NULL)
         OR rf.color_label = :color_label)
    AND (:keyword_id IS NULL OR EXISTS
         (SELECT 1 FROM image_keywords ik WHERE ik.image_id = i.id AND ik.keyword_id = :keyword_id))
    AND (:collection_id IS NULL OR EXISTS
         (SELECT 1 FROM collection_images ci WHERE ci.image_id = i.id AND ci.collection_id = :collection_id))
    AND (:import_session_id IS NULL OR i.import_session_id = :import_session_id)
    AND (:capture_year IS NULL OR strftime('%Y', i.capture_date, 'unixepoch') = :capture_year)
    AND (:capture_date IS NULL OR strftime('%Y-%m-%d', i.capture_date, 'unixepoch') = :capture_date)
    AND (:detected_category IS NULL
         OR EXISTS (SELECT 1 FROM image_detections d
                    WHERE d.image_id = i.id AND d.category = :detected_category)
         OR (:detected_category = 'People' AND EXISTS
              (SELECT 1 FROM image_user_labels ul
               WHERE ul.image_id = i.id AND ul.contains_person = 1))
         OR (:detected_category = 'Animals' AND EXISTS
              (SELECT 1 FROM image_user_labels ul
               WHERE ul.image_id = i.id AND ul.contains_animal = 1))
         OR (:detected_category = 'People' AND EXISTS
              (SELECT 1 FROM image_presence p
               WHERE p.image_id = i.id AND p.p_person >= :tau_person))
         OR (:detected_category = 'Animals' AND EXISTS
              (SELECT 1 FROM image_presence p
               WHERE p.image_id = i.id AND p.p_animal >= :tau_animal)))
    AND (:person_id IS NULL OR EXISTS
         (SELECT 1 FROM face fa WHERE fa.asset_id = i.id AND fa.person_id = :person_id
            AND fa.status IN ('confirmed','unconfirmed')))
    AND (:format IS NULL OR i.format = :format)
    AND (:suggested IS NULL OR EXISTS
         (SELECT 1 FROM image_suggestion sg WHERE sg.image_id = i.id
            AND sg.suggested = :suggested AND sg.withheld = 0))
    AND (COALESCE(:include_paired, 0) = 1
         OR NOT EXISTS (SELECT 1 FROM image_pairs ip WHERE ip.secondary_image_id = i.id))
    AND (:search IS NULL OR i.original_filename LIKE :search
                         OR i.camera_model LIKE :search
                         OR i.lens LIKE :search
                         OR EXISTS (SELECT 1 FROM image_keywords ik
                                    JOIN keywords k ON k.id = ik.keyword_id
                                    WHERE ik.image_id = i.id AND k.name LIKE :search))";

fn recon_sort_sql(sort: Option<&str>) -> &'static str {
    match sort {
        Some("capture_asc") => "i.capture_date ASC, i.id ASC",
        Some("filename") => "i.original_filename ASC, i.id ASC",
        Some("filename_desc") => "i.original_filename DESC, i.id DESC",
        Some("rating_desc") => "COALESCE(rf.stars,0) DESC, i.capture_date DESC, i.id DESC",
        Some("rating_asc") => "COALESCE(rf.stars,0) ASC, i.capture_date DESC, i.id DESC",
        Some("imported_desc") => "i.imported_at DESC, i.id DESC",
        Some("imported_asc") => "i.imported_at ASC, i.id ASC",
        _ => "i.capture_date DESC, i.id DESC",
    }
}

enum SeekKind {
    CaptureDesc,
    CaptureAsc,
    ImportedDesc,
    ImportedAsc,
}

fn recon_seek_kind(sort: Option<&str>) -> Option<SeekKind> {
    match sort {
        Some("capture_asc") => Some(SeekKind::CaptureAsc),
        Some("imported_desc") => Some(SeekKind::ImportedDesc),
        Some("imported_asc") => Some(SeekKind::ImportedAsc),
        Some("filename") | Some("filename_desc") | Some("rating_desc") | Some("rating_asc") => None,
        _ => Some(SeekKind::CaptureDesc),
    }
}

const NO_BINDS: &[(&str, &dyn ToSql)] = &[];

fn run_explain(
    conn: &Connection,
    sql: &str,
    binds: &[(&str, &dyn ToSql)],
) -> Result<Vec<String>, Err> {
    let plan_sql = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&plan_sql)?;
    let rows = stmt.query_map(binds, |r| r.get::<_, String>(3))?;
    Ok(rows.collect::<core_db::rusqlite::Result<Vec<_>>>()?)
}

/// Base filter binds shared by every `query_images`/`count_images` shape. `:tau_person`/
/// `:tau_animal` are bound to an arbitrary constant: every bench query leaves `detected_category`
/// unset, so the branches that read them are always dead (`:detected_category IS NULL` short-
/// circuits first) — their value cannot change the plan or the result.
fn base_binds<'a>(
    p: &'a QueryParams,
    search: &'a Option<String>,
    tau: &'a f64,
) -> Vec<(&'static str, &'a dyn ToSql)> {
    vec![
        (":folder_id", &p.folder_id),
        (":min_stars", &p.min_stars),
        (":flag", &p.flag),
        (":color_label", &p.color_label),
        (":keyword_id", &p.keyword_id),
        (":collection_id", &p.collection_id),
        (":import_session_id", &p.import_session_id),
        (":capture_year", &p.capture_year),
        (":capture_date", &p.capture_date),
        (":detected_category", &p.detected_category),
        (":person_id", &p.person_id),
        (":format", &p.format),
        (":suggested", &p.suggested),
        (":include_paired", &p.include_paired),
        (":tau_person", tau),
        (":tau_animal", tau),
        (":search", search),
    ]
}

fn explain_count(conn: &Connection, p: &QueryParams) -> Result<Vec<String>, Err> {
    let search = p.search.as_ref().map(|s| format!("%{s}%"));
    let tau = 0.5f64;
    let sql = format!(
        "SELECT COUNT(*) FROM images i LEFT JOIN ratings_flags rf ON rf.image_id = i.id WHERE {RECON_WHERE}"
    );
    run_explain(conn, &sql, &base_binds(p, &search, &tau))
}

/// Dispatches to the offset- or seek-shaped reconstruction depending on `p`, mirroring
/// `query_images`'s own dispatch (`p.seek == Some(true)` and a seek-eligible sort).
fn explain_query(conn: &Connection, p: &QueryParams) -> Result<Vec<String>, Err> {
    let search = p.search.as_ref().map(|s| format!("%{s}%"));
    let tau = 0.5f64;
    let binds = base_binds(p, &search, &tau);

    if p.seek == Some(true) {
        if let Some(kind) = recon_seek_kind(p.sort.as_deref()) {
            return explain_seek(conn, &binds, p, kind);
        }
    }

    let sql = format!(
        "SELECT {RECON_COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         {RECON_EDIT_JOIN}
         {RECON_SUGGEST_JOIN}
         WHERE {RECON_WHERE}
         ORDER BY {} LIMIT :limit OFFSET :offset",
        recon_sort_sql(p.sort.as_deref())
    );
    let limit = p.limit.unwrap_or(5000);
    let offset = p.offset.unwrap_or(0);
    let mut binds = binds;
    binds.push((":limit", &limit));
    binds.push((":offset", &offset));
    run_explain(conn, &sql, &binds)
}

/// One seek phase — the phase that actually runs given `p`'s cursor, mirroring `query.rs::
/// run_seek_phase`'s per-`SeekKind` branch selection (NULL-block vs non-NULL-block).
fn explain_seek(
    conn: &Connection,
    binds: &[(&str, &dyn ToSql)],
    p: &QueryParams,
    kind: SeekKind,
) -> Result<Vec<String>, Err> {
    let has_cursor = p.cursor_id.is_some();
    let has_cv = p.cursor_value.is_some();
    let (extra, order, use_cv, use_ci): (&str, &str, bool, bool) = match kind {
        SeekKind::CaptureDesc => {
            let in_null_block = has_cursor && !has_cv;
            if !in_null_block {
                if has_cv {
                    (
                        "i.capture_date IS NOT NULL AND (i.capture_date < :cv OR (i.capture_date = :cv AND i.id < :ci))",
                        "i.capture_date DESC, i.id DESC",
                        true,
                        true,
                    )
                } else {
                    (
                        "i.capture_date IS NOT NULL",
                        "i.capture_date DESC, i.id DESC",
                        false,
                        false,
                    )
                }
            } else {
                (
                    "i.capture_date IS NULL AND i.id < :ci",
                    "i.id DESC",
                    false,
                    true,
                )
            }
        }
        SeekKind::CaptureAsc => {
            let past_null_block = has_cv;
            if !past_null_block {
                if has_cursor {
                    (
                        "i.capture_date IS NULL AND i.id > :ci",
                        "i.id ASC",
                        false,
                        true,
                    )
                } else {
                    ("i.capture_date IS NULL", "i.id ASC", false, false)
                }
            } else {
                (
                    "i.capture_date IS NOT NULL AND (i.capture_date > :cv OR (i.capture_date = :cv AND i.id > :ci))",
                    "i.capture_date ASC, i.id ASC",
                    true,
                    true,
                )
            }
        }
        SeekKind::ImportedDesc => {
            if has_cursor {
                (
                    "(i.imported_at < :cv OR (i.imported_at = :cv AND i.id < :ci))",
                    "i.imported_at DESC, i.id DESC",
                    true,
                    true,
                )
            } else {
                ("1=1", "i.imported_at DESC, i.id DESC", false, false)
            }
        }
        SeekKind::ImportedAsc => {
            if has_cursor {
                (
                    "(i.imported_at > :cv OR (i.imported_at = :cv AND i.id > :ci))",
                    "i.imported_at ASC, i.id ASC",
                    true,
                    true,
                )
            } else {
                ("1=1", "i.imported_at ASC, i.id ASC", false, false)
            }
        }
    };

    let sql = format!(
        "SELECT {RECON_COLUMNS} FROM images i
         LEFT JOIN ratings_flags rf ON rf.image_id = i.id
         {RECON_EDIT_JOIN}
         {RECON_SUGGEST_JOIN}
         WHERE {RECON_WHERE} AND ({extra})
         ORDER BY {order} LIMIT :limit"
    );
    let limit = p.limit.unwrap_or(5000);
    let cv = p.cursor_value.unwrap_or(0);
    let ci = p.cursor_id.unwrap_or(0);
    let mut binds: Vec<(&str, &dyn ToSql)> = binds.to_vec();
    binds.push((":limit", &limit));
    if use_cv {
        binds.push((":cv", &cv));
    }
    if use_ci {
        binds.push((":ci", &ci));
    }
    run_explain(conn, &sql, &binds)
}

const RECON_LIST_FOLDERS_SQL: &str = r"SELECT f.id, f.path, COUNT(i.id)
         FROM folders f
         LEFT JOIN images i ON i.folder_id = f.id AND i.status = 'present'
              AND NOT EXISTS (SELECT 1 FROM image_pairs ip WHERE ip.secondary_image_id = i.id)
         GROUP BY f.id, f.path
         ORDER BY f.path";

const RECON_DATE_TREE_SQL: &str = r"SELECT COALESCE(strftime('%Y', capture_date, 'unixepoch'), 'Unknown')      AS y,
                COALESCE(strftime('%Y-%m-%d', capture_date, 'unixepoch'), 'Unknown') AS d,
                COUNT(*)
         FROM images i WHERE status = 'present'
           AND NOT EXISTS (SELECT 1 FROM image_pairs ip WHERE ip.secondary_image_id = i.id)
         GROUP BY y, d
         ORDER BY y = 'Unknown', y DESC, d = 'Unknown', d DESC";

const RECON_LIST_KEYWORDS_SQL: &str = r"SELECT k.id, k.name,
                (SELECT COUNT(*) FROM image_keywords ik
                 JOIN images i ON i.id = ik.image_id
                 WHERE ik.keyword_id = k.id AND i.status = 'present') AS cnt
         FROM keywords k
         ORDER BY k.name COLLATE NOCASE";

// `list_collections` (Rust side) additionally calls `count_images` per smart collection — that
// cost is already covered by the `count_images` plan above, so only the base listing is here.
const RECON_LIST_COLLECTIONS_SQL: &str =
    "SELECT id, name, is_smart, query FROM collections ORDER BY name COLLATE NOCASE";

// ================================================================================================
// Reporting
// ================================================================================================

fn print_table(table: &[Row]) {
    println!(
        "\n{:<10} {:<28} {:>10} {:>12}",
        "category", "operation", "min(ms)", "median(ms)"
    );
    println!("{}", "-".repeat(64));
    for r in table {
        println!(
            "{:<10} {:<28} {:>10.2} {:>12.2}",
            r.category, r.name, r.min_ms, r.median_ms
        );
    }
}

/// The starting SLOs from the task brief. "deep-page" has none stated — its numbers are printed in
/// the table above but never flagged here (deep OFFSET is expected to be worse; that's the point of
/// timing it separately, not a regression to alarm on until a real SLO is set for it).
fn budget_for(category: &str) -> Option<f64> {
    match category {
        "first-page" | "filter" | "search" | "sidebar" => Some(300.0),
        _ => None,
    }
}

fn print_over_budget(table: &[Row]) {
    println!("\n-- over budget (median > SLO) --");
    let offenders: Vec<&Row> = table
        .iter()
        .filter(|r| budget_for(r.category).is_some_and(|b| r.median_ms > b))
        .collect();
    if offenders.is_empty() {
        println!("  none");
        return;
    }
    for r in offenders {
        let budget = budget_for(r.category).unwrap();
        println!(
            "  {} / {}: median {:.2}ms > {:.0}ms budget",
            r.category, r.name, r.median_ms, budget
        );
    }
}
