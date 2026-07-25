//! Reference-mode import into a real catalog, without the GUI: register a folder as a library
//! root and index every supported file **in place** — the exact work `library_index_root` does
//! behind the app's "Reference" import. Nothing is copied, moved, or trashed.
//!
//!   cargo run --release -p core-library --example index_root -- <catalog.db> <thumbs-dir> <folder>
//!
//! Useful for importing a large card folder deterministically (a 38 GB / 2000-file corpus takes
//! minutes and thousands of thumbnails), and for re-running an import after a fix without driving
//! the UI.

use core_db::Db;
use core_library::{add_root, scan_root, ThumbCache, THUMB_SIZE};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let db_path = PathBuf::from(
        args.next()
            .expect("usage: index_root <catalog.db> <thumbs> <dir>"),
    );
    let thumbs_dir = PathBuf::from(
        args.next()
            .expect("usage: index_root <catalog.db> <thumbs> <dir>"),
    );
    let root = PathBuf::from(
        args.next()
            .expect("usage: index_root <catalog.db> <thumbs> <dir>"),
    )
    .canonicalize()?;

    let thumbs = ThumbCache::new(thumbs_dir)?;
    let mut db = Db::open(&db_path)?;
    let folder_id = add_root(&db.conn, &root)?;
    println!("root {} → folder {folder_id}", root.display());

    let t = Instant::now();
    let stats = scan_root(
        &mut db.conn,
        &thumbs,
        folder_id,
        &root,
        THUMB_SIZE,
        |done, total| {
            if done % 200 == 0 || done == total {
                println!("  {done}/{total}");
            }
        },
    )?;
    println!("{stats:?} in {:.1}s", t.elapsed().as_secs_f32());
    if stats.failed > 0 {
        println!("WARNING: {} files failed to index", stats.failed);
    }
    Ok(())
}
