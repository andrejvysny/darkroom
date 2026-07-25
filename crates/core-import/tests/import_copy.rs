//! Copy-import a few real CR3s into a temp library: verifies date routing, hash-verified copy,
//! catalog insertion, and idempotent re-import (no duplicates). Skips if `library/2026` is absent.

use core_db::Db;
use core_import::{dedup_scan, import, list_source, ImportMode, Pairing, SourceStatus};
use core_library::ThumbCache;
use std::collections::HashSet;
use std::path::PathBuf;
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
    let r2 = dedup_scan(&[c.clone()], &present_hashes, &present_sizes, |_, _, _| {});
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
