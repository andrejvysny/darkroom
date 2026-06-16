//! Full-scale Phase-1 validation: index an entire folder of CR3s into a catalog and verify
//! that every catalog row has a cached thumbnail. Run:
//!   cargo run -p core-library --example scan_library [DIR]

use core_db::Db;
use core_library::{add_root, query_images, scan_root, QueryParams, ThumbCache, THUMB_SIZE};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../library/2026"))
        .canonicalize()?;

    let tmp = std::env::temp_dir().join("darkroom-scan-validate");
    let _ = std::fs::remove_dir_all(&tmp);
    let thumbs = ThumbCache::new(tmp.join("thumbs"))?;
    let mut db = Db::open(&tmp.join("catalog.db"))?;
    let folder_id = add_root(&db.conn, &root)?;

    println!("Scanning {} …", root.display());
    let t = Instant::now();
    let stats = scan_root(
        &mut db.conn,
        &thumbs,
        folder_id,
        &root,
        THUMB_SIZE,
        |done, total| {
            if done == total || done % 20 == 0 {
                println!("  {done}/{total}");
            }
        },
    )?;
    let dt = t.elapsed();
    let per = dt.as_millis() as f64 / stats.scanned.max(1) as f64;
    println!("stats: {stats:?} in {dt:.1?} ({per:.0} ms/file wall, parallel)");

    let rows = query_images(&db.conn, &QueryParams::default())?;
    let thumb_ok = rows
        .iter()
        .filter(|r| thumbs.has(&r.content_hash, THUMB_SIZE))
        .count();
    println!(
        "query rows: {}  thumbs present: {}/{}",
        rows.len(),
        thumb_ok,
        rows.len()
    );

    // Re-scan must be idempotent (no new rows).
    let again = scan_root(
        &mut db.conn,
        &thumbs,
        folder_id,
        &root,
        THUMB_SIZE,
        |_, _| {},
    )?;
    println!("re-scan stats (expect added=0): {again:?}");

    assert_eq!(rows.len(), stats.added, "row count must equal added");
    assert_eq!(thumb_ok, rows.len(), "every row must have a thumbnail");
    assert_eq!(again.added, 0, "re-scan must add nothing (idempotent)");
    println!("VALIDATION OK ✓");
    Ok(())
}
