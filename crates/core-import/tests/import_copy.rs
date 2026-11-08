//! Copy-import a few real CR3s into a temp library: verifies date routing, hash-verified copy,
//! catalog insertion, and idempotent re-import (no duplicates). Skips if `library/2026` is absent.

use core_db::Db;
use core_import::{
    dedup_scan, import, import_files, list_source, ImportControl, ImportMode, NoSourceEvents,
    Pairing, RunToEnd, SourceEvents, SourceStatus,
};
use core_library::ThumbCache;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

fn library_files(n: usize) -> Vec<PathBuf> {
    let dir = match PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026/2026-06-06")
        .canonicalize()
    {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .and_then(|s| s.to_str())
                .map(|s| s.eq_ignore_ascii_case("cr3"))
                .unwrap_or(false)
        })
        .collect();
    v.sort();
    v.truncate(n);
    v
}

#[test]
fn copy_import_routes_and_dedupes() {
    let files = library_files(3);
    if files.is_empty() {
        eprintln!("library/2026 not present — skipping");
        return;
    }
    // The full 240-file library is not committed (only reference fixtures are), so assert
    // against however many CR3s are actually present rather than a hardcoded count.
    let n = files.len();

    let card = tempfile::tempdir().unwrap();
    for f in &files {
        std::fs::copy(f, card.path().join(f.file_name().unwrap())).unwrap();
    }
    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import(
        &db,
        &thumbs,
        card.path(),
        ImportMode::Copy,
        libdir.path(),
        true,
        Pairing::Standalone,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();
    assert_eq!(stats.added, n, "all available files imported");
    assert_eq!(stats.skipped, 0);
    assert_eq!(stats.failed, 0);

    let routed: Vec<String> = {
        let g = db.lock().unwrap();
        let mut stmt = g
            .conn
            .prepare("SELECT path FROM images ORDER BY id")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.filter_map(Result::ok).collect()
    };
    assert_eq!(routed.len(), n);
    for p in &routed {
        assert!(p.contains("/2026/2026-06-06/"), "date-routed: {p}");
        assert!(std::path::Path::new(p).exists(), "copied file exists: {p}");
    }

    // Re-import the same card → byte-identical, must skip all.
    let again = import(
        &db,
        &thumbs,
        card.path(),
        ImportMode::Copy,
        libdir.path(),
        true,
        Pairing::Standalone,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();
    assert_eq!(again.added, 0, "idempotent re-import adds nothing");
    assert_eq!(again.skipped, n);

    let count: i64 = db
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, n as i64, "no duplicate rows");
}

/// Stage a card holding one RAW + JPEG pair (`PAIR01.CR3` + `PAIR01.JPG`), from the committed
/// fixtures. `None` when the CR3 fixture is absent.
fn pair_card() -> Option<tempfile::TempDir> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let raw = root
        .join("library/2026/2026-06-06/_55A3947.CR3")
        .canonicalize()
        .ok()?;
    let jpeg = root.join("docs/sample-poppies.jpg").canonicalize().ok()?;
    let card = tempfile::tempdir().unwrap();
    std::fs::copy(&raw, card.path().join("PAIR01.CR3")).unwrap();
    std::fs::copy(&jpeg, card.path().join("PAIR01.JPG")).unwrap();
    Some(card)
}

fn run_pair_import(card: &std::path::Path, pairing: Pairing) -> (Mutex<Db>, tempfile::TempDir) {
    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let stats = import(
        &db,
        &thumbs,
        card,
        ImportMode::Copy,
        libdir.path(),
        true,
        pairing,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();
    assert_eq!(stats.added, 2, "both members of the pair are catalogued");
    assert_eq!(
        stats.paired,
        usize::from(pairing == Pairing::Pair),
        "companion linked only under Pairing::Pair"
    );
    (db, libdir)
}

#[test]
fn pair_import_links_companion_and_hides_it_from_the_grid() {
    let Some(card) = pair_card() else {
        eprintln!("CR3 fixture not present — skipping");
        return;
    };

    // Standalone: two independent rows, both visible, no link.
    let (db, _lib) = run_pair_import(card.path(), Pairing::Standalone);
    {
        let g = db.lock().unwrap();
        let links: i64 = g
            .conn
            .query_row("SELECT COUNT(*) FROM image_pairs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(links, 0, "standalone import must not pair anything");
        let visible =
            core_library::query_images(&g.conn, &core_library::QueryParams::default()).unwrap();
        assert_eq!(visible.len(), 2, "both files show in the grid");
    }

    // Pair: the JPEG is linked to the CR3 and drops out of the default grid.
    let (db, _lib) = run_pair_import(card.path(), Pairing::Pair);
    let g = db.lock().unwrap();
    let visible =
        core_library::query_images(&g.conn, &core_library::QueryParams::default()).unwrap();
    assert_eq!(visible.len(), 1, "the pair occupies one grid cell");
    let primary = &visible[0];
    assert_eq!(primary.filename, "PAIR01.CR3", "the RAW is the primary");
    assert_eq!(primary.paired_count, 1);

    let info = core_library::pair_info(&g.conn, primary.id)
        .unwrap()
        .unwrap();
    assert_eq!(info.role, "primary");
    assert_eq!(info.secondaries.len(), 1);
    assert_eq!(info.secondaries[0].filename, "PAIR01.JPG");

    // The companion is still a real, queryable catalog row — just hidden by default.
    let all = core_library::query_images(
        &g.conn,
        &core_library::QueryParams {
            include_paired: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(all.len(), 2);
}

// `list_source` + `dedup_scan` only touch raw bytes (enumerate by extension, hash via BLAKE3 — no
// RAW decode), so these run on synthetic `.cr3` files with no real-camera fixture needed.

#[test]
fn list_source_lists_pending_and_honors_recursion() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("A.CR3"), b"AAA").unwrap();
    std::fs::write(dir.path().join("B.CR3"), b"BBBBB").unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/C.CR3"), b"CCCCCCC").unwrap();

    // Recursive sees the subfolder file; all start Pending with sizes populated, no hashing.
    let deep = list_source(dir.path(), true);
    assert_eq!(deep.len(), 3);
    assert!(deep.iter().all(|f| f.status == SourceStatus::Pending));
    assert!(deep
        .iter()
        .all(|f| f.size_bytes > 0 && f.filename.ends_with(".CR3")));

    // Non-recursive excludes the subfolder.
    let top = list_source(dir.path(), false);
    assert_eq!(top.len(), 2);
    assert!(top.iter().all(|f| f.filename != "C.CR3"));
}

#[test]
fn dedup_scan_classifies_by_content_hash() {
    let dir = tempfile::tempdir().unwrap();
    // A and B share content (same bytes → same size → same hash); C has a unique size.
    std::fs::write(dir.path().join("A.CR3"), b"SAME").unwrap();
    std::fs::write(dir.path().join("B.CR3"), b"SAME").unwrap();
    std::fs::write(dir.path().join("C.CR3"), b"UNIQUE-CONTENT").unwrap();
    let a = dir.path().join("A.CR3");
    let b = dir.path().join("B.CR3");
    let c = dir.path().join("C.CR3");

    // Empty catalog: A new, B a batch duplicate of A, C new (unique size → not even hashed).
    let r = dedup_scan(
        &[a.clone(), b.clone(), c.clone()],
        &HashSet::new(),
        &HashSet::new(),
        |_, _, _| {},
    );
    let by = |p: &std::path::Path| {
        r.iter()
            .find(|d| d.path == p.display().to_string())
            .unwrap()
            .status
    };
    assert_eq!(by(&a), SourceStatus::New);
    assert_eq!(by(&b), SourceStatus::DuplicateBatch);
    assert_eq!(by(&c), SourceStatus::New);

    // Catalog already holds C's content (by hash + size) → C is a library duplicate.
    let c_hash = core_raw::content_hash(b"UNIQUE-CONTENT");
    let present_hashes: HashSet<[u8; 32]> = [c_hash].into_iter().collect();
    let present_sizes: HashSet<i64> = ["UNIQUE-CONTENT".len() as i64].into_iter().collect();
    let r2 = dedup_scan(
        std::slice::from_ref(&c),
        &present_hashes,
        &present_sizes,
        |_, _, _| {},
    );
    assert_eq!(r2[0].status, SourceStatus::DuplicateLibrary);
}

/// End-to-end Move import: the source originals must be gone (trashed) only AFTER their verified
/// copies are catalogued. Ignored by default because it sends real files to the macOS Trash — run
/// explicitly with `cargo test -p core-import -- --ignored`.
#[test]
#[ignore = "sends source files to the real macOS Trash; run explicitly"]
fn move_import_trashes_sources_after_catalog() {
    let files = library_files(2);
    if files.is_empty() {
        eprintln!("library/2026 not present — skipping");
        return;
    }
    let n = files.len();

    let card = tempfile::tempdir().unwrap();
    let sources: Vec<PathBuf> = files
        .iter()
        .map(|f| {
            let dst = card.path().join(f.file_name().unwrap());
            std::fs::copy(f, &dst).unwrap();
            dst
        })
        .collect();

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import(
        &db,
        &thumbs,
        card.path(),
        ImportMode::Move,
        libdir.path(),
        true,
        Pairing::Standalone,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();

    assert_eq!(stats.added, n, "all files moved into the library");
    assert_eq!(stats.failed, 0);
    assert_eq!(stats.source_retained, 0, "every source was trashed");

    // Sources gone (in Trash); destinations exist and are catalogued.
    for s in &sources {
        assert!(!s.exists(), "source removed after move: {}", s.display());
    }
    let routed: Vec<String> = {
        let g = db.lock().unwrap();
        let mut stmt = g.conn.prepare("SELECT path FROM images").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.filter_map(Result::ok).collect()
    };
    assert_eq!(routed.len(), n);
    for p in &routed {
        assert!(std::path::Path::new(p).exists(), "library copy exists: {p}");
    }
}

// ---------------------------------------------------------------------------------------------
// Parallel commit path. These run on the committed `docs/sample-poppies.jpg` (a supported, real,
// decodable format) rather than a camera RAW, so they need no uncommitted fixture and still drive
// the FULL pipeline: metadata probe, hash-verified copy, decode, thumbnail, catalog insert.
// ---------------------------------------------------------------------------------------------

fn poppies() -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/sample-poppies.jpg");
    std::fs::read(&p).expect("committed fixture docs/sample-poppies.jpg")
}

/// The same JPEG with `tag` appended. A JPEG decoder stops at the EOI marker, so the image still
/// decodes identically while the file is byte- (and therefore hash-) distinct.
fn variant(base: &[u8], tag: &str) -> Vec<u8> {
    let mut v = base.to_vec();
    v.extend_from_slice(tag.as_bytes());
    v
}

/// Stage a synthetic card that exercises every rule the parallel commit has to preserve:
/// 30 unique files, 5 byte-identical pairs (10 files, distinct filenames — so the duplicate is
/// caught by content, not by a destination collision), and 2 same-FILENAME different-content files
/// in separate subfolders (so both race for `SAME.jpg` in the same date folder).
/// 42 files, 37 distinct contents. Returns the staged paths.
fn stage_card(dir: &Path) -> Vec<PathBuf> {
    let base = poppies();
    let mut staged: Vec<PathBuf> = Vec::new();
    let mut write = |rel: &str, bytes: &[u8]| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        staged.push(p);
    };
    for i in 0..30 {
        write(
            &format!("uniq{i:02}.jpg"),
            &variant(&base, &format!("u{i:02}")),
        );
    }
    for i in 0..5 {
        let dup = variant(&base, &format!("dup{i}"));
        write(&format!("dup{i}_a.jpg"), &dup);
        write(&format!("dup{i}_b.jpg"), &dup);
    }
    write("a/SAME.jpg", &variant(&base, "same-a"));
    write("b/SAME.jpg", &variant(&base, "same-b"));
    staged
}

/// Every regular file under `root`, recursively.
fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_files(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn catalog_paths(db: &Mutex<Db>) -> Vec<String> {
    let g = db.lock().unwrap();
    let mut stmt = g
        .conn
        .prepare("SELECT path FROM images ORDER BY id")
        .unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
    rows.filter_map(Result::ok).collect()
}

/// The chunked/parallel commit must produce exactly what the old file-at-a-time loop did: identical
/// content is catalogued once, the redundant copy it made is deleted (no orphans, no `*.part`), and
/// two files sharing a filename get distinct destinations instead of overwriting each other.
#[test]
fn parallel_copy_import_keeps_catalog_rules() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_card(card.path());
    assert_eq!(staged.len(), 42);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import(
        &db,
        &thumbs,
        card.path(),
        ImportMode::Copy,
        libdir.path(),
        true,
        Pairing::Standalone,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();

    assert_eq!(stats.total, 42);
    assert_eq!(stats.added, 37, "one row per distinct content");
    assert_eq!(stats.skipped, 5, "the second file of each identical pair");
    assert_eq!(stats.failed, 0);

    let count: i64 = db
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 37);

    let routed = catalog_paths(&db);
    assert_eq!(routed.len(), 37);
    for p in &routed {
        assert!(Path::new(p).exists(), "catalogued file exists: {p}");
    }

    // No orphan copy of an in-chunk duplicate, and no temp file left behind.
    let on_disk = walk_files(libdir.path());
    assert!(
        on_disk
            .iter()
            .all(|p| !p.to_string_lossy().ends_with(".part")),
        "no `*.part` temp files survive the import"
    );
    assert_eq!(
        on_disk.len(),
        37,
        "the library holds exactly the catalogued files: {on_disk:#?}"
    );

    // `a/SAME.jpg` and `b/SAME.jpg` differ in content, so both are imported and the second is
    // renamed — neither may overwrite the other even though they are copied concurrently.
    let names: Vec<String> = on_disk
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"SAME.jpg".to_string()), "{names:#?}");
    assert!(names.contains(&"SAME_1.jpg".to_string()), "{names:#?}");
    let same_dirs: HashSet<PathBuf> = on_disk
        .iter()
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            n == "SAME.jpg" || n == "SAME_1.jpg"
        })
        .map(|p| p.parent().unwrap().to_path_buf())
        .collect();
    assert_eq!(same_dirs.len(), 1, "both land in the same date folder");
}

/// The in-chunk duplicate rule, forced deterministically: two identical files with DIFFERENT names
/// (so no destination collision short-circuits them) and a card small enough that both are certain
/// to be in the same parallel chunk. Both get copied — the second cannot see the first's hash reach
/// `seen` — so the catalog step has to count it skipped AND delete the redundant copy.
#[test]
fn in_chunk_duplicate_is_skipped_and_its_copy_removed() {
    let card = tempfile::tempdir().unwrap();
    let content = variant(&poppies(), "twin");
    std::fs::write(card.path().join("A.jpg"), &content).unwrap();
    std::fs::write(card.path().join("B.jpg"), &content).unwrap();

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import(
        &db,
        &thumbs,
        card.path(),
        ImportMode::Copy,
        libdir.path(),
        true,
        Pairing::Standalone,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();
    assert_eq!((stats.added, stats.skipped, stats.failed), (1, 1, 0));

    let on_disk = walk_files(libdir.path());
    assert_eq!(on_disk.len(), 1, "the redundant copy is gone: {on_disk:#?}");
    assert_eq!(catalog_paths(&db), vec![on_disk[0].display().to_string()]);
    // Neither original was touched (Copy mode never trashes, and a duplicate never trashes at all).
    assert_eq!(walk_files(card.path()).len(), 2);
}

/// Reference mode catalogs the files where they already are: nothing is copied, the parallel phase
/// must not fabricate library paths, and identical content is still deduped.
#[test]
fn reference_import_reads_in_place() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_card(card.path());
    assert_eq!(staged.len(), 42);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import(
        &db,
        &thumbs,
        card.path(),
        ImportMode::Reference,
        libdir.path(),
        true,
        Pairing::Standalone,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();

    assert_eq!(stats.added, 37);
    assert_eq!(stats.skipped, 5);
    assert_eq!(stats.failed, 0);

    let card_prefix = card.path().display().to_string();
    for p in catalog_paths(&db) {
        assert!(p.starts_with(&card_prefix), "referenced in place: {p}");
        assert!(Path::new(&p).exists());
    }
    // Nothing was written into the library, and the user's own files are all still there.
    assert!(walk_files(libdir.path()).is_empty());
    assert_eq!(walk_files(card.path()).len(), 42);
}

/// Hashing a batch in parallel must not disturb the verdicts, which are order-dependent: the FIRST
/// occurrence of a content is `New` and every later one `DuplicateBatch`. Progress must also stay
/// monotonic and finish on the total.
#[test]
fn dedup_scan_preserves_first_occurrence_order() {
    let dir = tempfile::tempdir().unwrap();
    // Same length everywhere, so the size prefilter hashes all of them (no free `New`s).
    for name in ["dupA.CR3", "dupA2.CR3", "dupA3.CR3"] {
        std::fs::write(dir.path().join(name), b"IDENTICAL-BYTES").unwrap();
    }
    std::fs::write(dir.path().join("uniq.CR3"), b"DIFFERENT-BYTES").unwrap();
    let paths: Vec<PathBuf> = ["dupA.CR3", "dupA2.CR3", "dupA3.CR3", "uniq.CR3"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();

    let seen = Mutex::new(Vec::<(usize, usize)>::new());
    let r = dedup_scan(
        &paths,
        &HashSet::new(),
        &HashSet::new(),
        |done, total, _| {
            seen.lock().unwrap().push((done, total));
        },
    );

    let statuses: Vec<SourceStatus> = r.iter().map(|d| d.status).collect();
    assert_eq!(
        statuses,
        vec![
            SourceStatus::New,
            SourceStatus::DuplicateBatch,
            SourceStatus::DuplicateBatch,
            SourceStatus::New
        ]
    );
    let out_paths: Vec<String> = r.iter().map(|d| d.path.clone()).collect();
    let in_paths: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
    assert_eq!(out_paths, in_paths, "results stay in input order");

    // A batch bigger than one chunk: progress fires per chunk, strictly increasing, ending on total.
    let big_dir = tempfile::tempdir().unwrap();
    let big: Vec<PathBuf> = (0..50)
        .map(|i| {
            let p = big_dir.path().join(format!("f{i:02}.CR3"));
            std::fs::write(&p, b"ALL-THE-SAME").unwrap();
            p
        })
        .collect();
    let ticks = Mutex::new(Vec::<usize>::new());
    let r2 = dedup_scan(&big, &HashSet::new(), &HashSet::new(), |done, total, _| {
        assert_eq!(total, 50);
        ticks.lock().unwrap().push(done);
    });
    assert_eq!(r2[0].status, SourceStatus::New);
    assert!(r2[1..]
        .iter()
        .all(|d| d.status == SourceStatus::DuplicateBatch));
    let ticks = ticks.into_inner().unwrap();
    assert!(ticks.len() > 1, "more than one chunk: {ticks:?}");
    assert!(ticks.windows(2).all(|w| w[1] > w[0]), "{ticks:?}");
    assert_eq!(*ticks.last().unwrap(), 50);
}

/// `process_bytes` is `process_file` minus the read+hash — the split the importer relies on to read
/// each source once. Both must describe the file identically.
#[test]
fn process_bytes_matches_process_file() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("poppies.jpg");
    std::fs::write(&src, poppies()).unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();

    let via_file = core_library::process_file(&src, &thumbs, core_library::THUMB_SIZE).unwrap();

    let bytes = std::sync::Arc::new(std::fs::read(&src).unwrap());
    let digest = core_raw::content_hash(&bytes);
    let via_bytes =
        core_library::process_bytes(&src, bytes, digest, &thumbs, core_library::THUMB_SIZE, None)
            .unwrap();

    assert_eq!(via_file.content_hash_hex, via_bytes.content_hash_hex);
    assert_eq!(via_file.file_size, via_bytes.file_size);
    assert_eq!(
        (via_file.width, via_file.height),
        (via_bytes.width, via_bytes.height)
    );
    assert_eq!(via_file.meta.capture_date, via_bytes.meta.capture_date);
}

// ---------------------------------------------------------------------------------------------
// Cooperative stop seam (`ImportControl`). The chunk boundary is the only stop point, so these
// pin the contract: nothing half-processed, honest counts, and a finished session row either way.
// ---------------------------------------------------------------------------------------------

/// `n` byte-distinct JPEGs (the poppies fixture plus a per-file tag), so every file is importable
/// and unique — each selected file must land in `added`, none deduped.
fn stage_unique(dir: &Path, n: usize) -> Vec<PathBuf> {
    let base = poppies();
    (0..n)
        .map(|i| {
            let p = dir.join(format!("u{i:03}.jpg"));
            std::fs::write(&p, variant(&base, &format!("v{i:03}"))).unwrap();
            p
        })
        .collect()
}

/// A host that already wants out: the check lands before chunk 1, before anything is processed.
struct StopAlways;
impl ImportControl for StopAlways {
    fn should_stop(&self) -> bool {
        true
    }
}

/// Counts `should_stop` calls and stops the second time it is asked — chunk 1 runs and commits,
/// chunk 2 never starts. Also counts `committed()` calls to pin the once-only contract.
struct StopAfterTwoAsks {
    asks: AtomicUsize,
    committed: AtomicUsize,
}
impl StopAfterTwoAsks {
    fn new() -> Self {
        Self {
            asks: AtomicUsize::new(0),
            committed: AtomicUsize::new(0),
        }
    }
}
impl ImportControl for StopAfterTwoAsks {
    fn should_stop(&self) -> bool {
        // 1st ask (before chunk 1): let it run. 2nd ask (before chunk 2): stop.
        self.asks.fetch_add(1, Ordering::SeqCst) + 1 >= 2
    }
    fn committed(&self) {
        self.committed.fetch_add(1, Ordering::SeqCst);
    }
}

/// `(finished_at, file_count, skipped_count)` of the run's session row.
fn session_row(db: &Mutex<Db>, id: i64) -> (Option<i64>, i64, i64) {
    let g = db.lock().unwrap();
    g.conn
        .query_row(
            &format!(
                "SELECT finished_at, file_count, skipped_count FROM import_sessions WHERE id={id}"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

#[test]
fn stop_before_first_chunk_commits_nothing() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), 5);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &StopAlways,
    )
    .unwrap();

    assert_eq!(stats.added, 0, "a stopped-before-start run adds nothing");
    assert!(stats.interrupted, "the host request is recorded");
    assert_eq!(stats.not_processed, staged.len(), "nothing was reached");
    assert!(
        walk_files(libdir.path()).is_empty(),
        "no file may enter the library"
    );
    let (finished_at, file_count, skipped) = session_row(&db, stats.session_id);
    assert!(
        finished_at.is_some(),
        "the session row exists and is finished"
    );
    assert_eq!((file_count, skipped), (0, 0));
}

#[test]
fn stop_between_chunks_keeps_committed_rows() {
    // More than CHUNK (16) unique files: the first chunk runs and commits, the run stops before
    // the second chunk is ever processed.
    const CHUNK_FILES: usize = 16;
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), CHUNK_FILES + 4);
    let total = staged.len();

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let control = StopAfterTwoAsks::new();

    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &control,
    )
    .unwrap();

    assert!(stats.interrupted);
    assert_eq!(
        stats.added + stats.skipped + stats.failed,
        CHUNK_FILES,
        "exactly one chunk was processed"
    );
    assert_eq!(stats.added, CHUNK_FILES, "all contents are distinct");
    assert_eq!(stats.not_processed, total - CHUNK_FILES);
    assert_eq!(
        control.committed.load(Ordering::SeqCst),
        1,
        "committed() fires exactly once, on the first catalog insert"
    );

    let rows = catalog_paths(&db);
    assert_eq!(rows.len(), stats.added, "DB rows match `added`");
    for p in &rows {
        assert!(
            Path::new(p).exists(),
            "committed row has its library copy: {p}"
        );
    }
    assert_eq!(
        walk_files(libdir.path()).len(),
        stats.added,
        "the never-started chunk left nothing in the library"
    );
}

#[test]
fn run_to_end_unchanged() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), 20);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();

    assert!(
        !stats.interrupted,
        "an uninterrupted run reports no interruption"
    );
    assert_eq!(stats.not_processed, 0);
    assert_eq!(
        stats.added, 20,
        "the full run spans >1 chunk and is unaffected"
    );
}

// ---------------------------------------------------------------------------------------------
// Publication fence (`ImportControl::commit`). Every catalog write after the unlocked phase runs
// inside the fence; a false answer means the catalog this run started on is gone — nothing more may
// be written, nothing may be trashed, and the run reports honestly instead of erroring.
// ---------------------------------------------------------------------------------------------

/// A host whose catalog was reset the moment this run started: every fence check fails.
struct StaleAlways;
impl ImportControl for StaleAlways {
    fn should_stop(&self) -> bool {
        false
    }
    fn commit(&self, _commit: &mut dyn FnMut(&mut dyn SourceEvents)) -> bool {
        false
    }
}

/// A host whose catalog goes stale after `allowed` commits: the first `allowed` fences pass (and
/// run the write), every later one refuses without running it.
struct StaleAfterN {
    allowed: usize,
    calls: AtomicUsize,
}
impl ImportControl for StaleAfterN {
    fn should_stop(&self) -> bool {
        false
    }
    fn commit(&self, commit: &mut dyn FnMut(&mut dyn SourceEvents)) -> bool {
        if self.calls.fetch_add(1, Ordering::SeqCst) < self.allowed {
            commit(&mut NoSourceEvents);
            true
        } else {
            false
        }
    }
}

/// A host that holds the fence, runs every commit, and records the `source_changed` identity events.
struct RecordingControl {
    events: Mutex<Vec<(i64, String, String)>>,
}
impl RecordingControl {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }
}
struct Recorder<'a> {
    out: &'a Mutex<Vec<(i64, String, String)>>,
}
impl SourceEvents for Recorder<'_> {
    fn source_changed(&mut self, image_id: i64, content_hash_hex: &str, path: &str) {
        self.out
            .lock()
            .unwrap()
            .push((image_id, content_hash_hex.to_string(), path.to_string()));
    }
}
impl ImportControl for RecordingControl {
    fn should_stop(&self) -> bool {
        false
    }
    fn commit(&self, commit: &mut dyn FnMut(&mut dyn SourceEvents)) -> bool {
        commit(&mut Recorder { out: &self.events });
        true
    }
}

#[test]
fn stale_catalog_before_commit_writes_nothing_and_keeps_originals() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), 3);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Move,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &StaleAlways,
    )
    .unwrap();

    assert_eq!(stats.added, 0, "a stale run adds nothing");
    assert!(stats.interrupted);
    for src in &staged {
        assert!(
            src.exists(),
            "a Move source is NEVER trashed uncommitted: {src:#?}"
        );
    }
    assert!(
        walk_files(libdir.path()).is_empty(),
        "no copy survives in the library: {:?}",
        walk_files(libdir.path())
    );
    let count: i64 = db
        .lock()
        .unwrap()
        .conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "the catalog holds nothing from this run");
}

#[test]
fn stale_catalog_mid_run_keeps_earlier_commits() {
    const COMMITTED_BEFORE_STALE: usize = 5;
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), 20);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());
    let control = StaleAfterN {
        allowed: COMMITTED_BEFORE_STALE,
        calls: AtomicUsize::new(0),
    };

    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &control,
    )
    .unwrap();

    assert_eq!(stats.added, COMMITTED_BEFORE_STALE, "earlier commits stand");
    assert!(stats.interrupted);
    assert_eq!(stats.not_processed, 20 - COMMITTED_BEFORE_STALE);

    let rows = catalog_paths(&db);
    assert_eq!(rows.len(), COMMITTED_BEFORE_STALE, "DB rows match `added`");
    for p in &rows {
        assert!(
            Path::new(p).exists(),
            "committed row has its library copy: {p}"
        );
    }
    assert_eq!(
        walk_files(libdir.path()).len(),
        COMMITTED_BEFORE_STALE,
        "the stale file's copy and the uncommitted rest of its chunk are gone"
    );
}

#[test]
fn relink_reports_source_event() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), 1);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &RunToEnd,
    )
    .unwrap();
    let (row_id, hash_hex): (i64, String) = {
        let g = db.lock().unwrap();
        let (id, hash): (i64, Vec<u8>) = g
            .conn
            .query_row("SELECT id, content_hash FROM images", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        (id, hash.iter().map(|b| format!("{b:02x}")).collect())
    };
    assert_eq!(hash_hex.len(), 64, "lowercase 64-char hex");

    // Externally invalidate the row: reconcile marks it `missing`, then its copy is deleted.
    let lib_copy = {
        let g = db.lock().unwrap();
        g.conn
            .execute(
                &format!("UPDATE images SET status = 'missing' WHERE id = {row_id}"),
                [],
            )
            .unwrap();
        g.conn
            .query_row(
                &format!("SELECT path FROM images WHERE id = {row_id}"),
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
    };
    std::fs::remove_file(&lib_copy).unwrap();

    // Re-import the same bytes; the missing row must be relinked AND the event reported.
    let control = RecordingControl::new();
    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &control,
    )
    .unwrap();
    assert_eq!(stats.added, 1, "the relink counts as added");

    let events = control.events.into_inner().unwrap();
    assert_eq!(events.len(), 1, "exactly one relink event");
    let (event_id, event_hash, event_path) = &events[0];
    assert_eq!(*event_id, row_id, "the original row id is repointed");
    assert_eq!(
        event_hash, &hash_hex,
        "event carries the row's content-hash hex"
    );
    assert_eq!(
        event_path, &lib_copy,
        "event reports where the row's bytes live now"
    );
    assert!(
        Path::new(event_path).exists(),
        "and the file is really there"
    );
}

#[test]
fn fresh_import_reports_one_source_event_per_added_row() {
    let card = tempfile::tempdir().unwrap();
    let staged = stage_unique(card.path(), 3);

    let libdir = tempfile::tempdir().unwrap();
    let thumbdir = tempfile::tempdir().unwrap();
    let thumbs = ThumbCache::new(thumbdir.path()).unwrap();
    let db = Mutex::new(Db::open_in_memory().unwrap());

    let control = RecordingControl::new();
    let stats = import_files(
        &db,
        &thumbs,
        card.path(),
        &staged,
        ImportMode::Copy,
        libdir.path(),
        Pairing::Standalone,
        None,
        |_, _, _| {},
        &control,
    )
    .unwrap();
    assert_eq!(stats.added, 3);

    // A fresh insert is a source identity change too: one event per added row, carrying exactly
    // the (id, hash hex, path) that was committed. Without this, a REUSED rowid (no AUTOINCREMENT)
    // stays unregistered in the host's gate and every later publication for it goes Stale.
    let rows: Vec<(i64, String, String)> = {
        let g = db.lock().unwrap();
        let mut stmt = g
            .conn
            .prepare("SELECT id, content_hash, path FROM images")
            .unwrap();
        let mut out: Vec<(i64, String, String)> = stmt
            .query_map([], |r| {
                let hash: Vec<u8> = r.get(1)?;
                Ok((
                    r.get(0)?,
                    hash.iter().map(|b| format!("{b:02x}")).collect(),
                    r.get(2)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        out.sort();
        out
    };
    let mut events = control.events.into_inner().unwrap();
    events.sort();
    assert_eq!(events.len(), 3, "one event per added row");
    assert_eq!(events, rows, "events match the committed identities");
    for (_, _, path) in &events {
        assert!(
            Path::new(path).exists(),
            "event path is the live library copy"
        );
    }
}
