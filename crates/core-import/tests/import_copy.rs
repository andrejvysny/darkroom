//! Copy-import a few real CR3s into a temp library: verifies date routing, hash-verified copy,
//! catalog insertion, and idempotent re-import (no duplicates). Skips if `library/2026` is absent.

use core_db::Db;
use core_import::{import, ImportMode};
use core_library::ThumbCache;
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
