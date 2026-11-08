//! Index a few real CR3s into an in-memory catalog and query them back.
//! Skips gracefully if `library/2026` is absent.

use core_db::Db;
use core_library::{
    add_root, insert_image, now_epoch, process_file, query_images, QueryParams, ThumbCache,
};
use std::path::PathBuf;

fn library_dir() -> Option<PathBuf> {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026/2026-06-06")
        .canonicalize()
        .ok()
        .filter(|p| p.exists())
}

fn first_n_cr3(dir: &PathBuf, n: usize) -> Vec<PathBuf> {
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
fn index_and_query_real_cr3() {
    let Some(dir) = library_dir() else {
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let files = first_n_cr3(&dir, 3);
    if files.is_empty() {
        eprintln!("no CR3 in {dir:?} — skipping");
        return;
    }

    let tmp = std::env::temp_dir().join(format!("darkroom-thumbs-test-{}", std::process::id()));
    let thumbs = ThumbCache::new(&tmp).unwrap();

    let mut db = Db::open_in_memory().unwrap();
    let folder_id = add_root(&db.conn, &dir).unwrap();

    let imported_at = now_epoch();
    let mut added = 0;
    for f in &files {
        let p = process_file(f, &thumbs, 256).expect("process");
        assert!(
            p.width >= 4000 && p.height >= 3000,
            "dims {p:?}",
            p = (p.width, p.height)
        );
        assert!(
            thumbs.has(&p.content_hash_hex, 256),
            "thumb written to cache"
        );
        if insert_image(&db.conn, folder_id, imported_at, &p)
            .unwrap()
            .is_some()
        {
            added += 1;
        }
    }
    assert_eq!(added, files.len(), "all distinct files inserted");

    // Re-insert the first file → byte-identical duplicate must be skipped.
    let dup = process_file(&files[0], &thumbs, 256).unwrap();
    assert!(insert_image(&db.conn, folder_id, imported_at, &dup)
        .unwrap()
        .is_none());

    let rows = query_images(&db.conn, &QueryParams::default()).unwrap();
    assert_eq!(rows.len(), files.len());
    let r = &rows[0];
    assert_eq!(r.content_hash.len(), 64, "hex hash");
    assert!(r.camera_model.as_deref().unwrap_or("").contains("R7"));
    assert!(r.width.unwrap() >= 4000);

    // pull connection mutable-free count
    let _ = &mut db;
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn process_bytes_writes_embedded_preview_and_survives_its_failure() {
    let Some(dir) = library_dir() else {
        eprintln!("library/2026 not present — skipping");
        return;
    };
    let Some(path) = first_n_cr3(&dir, 1).pop() else {
        return;
    };
    let bytes = std::sync::Arc::new(std::fs::read(&path).unwrap());
    let digest = core_raw::content_hash(&bytes);
    let hex = core_raw::hex(&digest);
    let root = std::env::temp_dir().join(format!("darkroom-emb-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let thumbs = ThumbCache::new(&root).unwrap();

    let p = core_library::process_bytes(&path, bytes.clone(), digest, &thumbs, 512, Some(2560))
        .expect("process with preview");
    assert!(thumbs.has(&hex, 512));
    let emb = thumbs
        .read_embedded_preview(&hex, 2560)
        .expect("emb written");
    assert_eq!(&emb[..2], &[0xFF, 0xD8]);
    // The grid thumb is the same bytes as without a preview request.
    let plain_root = root.join("plain");
    let plain = ThumbCache::new(&plain_root).unwrap();
    core_library::process_bytes(&path, bytes.clone(), digest, &plain, 512, None).unwrap();
    assert_eq!(
        thumbs.read(&hex, 512).unwrap(),
        plain.read(&hex, 512).unwrap()
    );
    assert!(!plain.has_embedded_preview(&hex, 2560));

    // A directory squatting on the preview path makes that write fail; the import must not.
    let broken = ThumbCache::new(root.join("broken")).unwrap();
    std::fs::create_dir(broken.embedded_preview_path_for(&hex, 2560)).unwrap();
    let q = core_library::process_bytes(&path, bytes, digest, &broken, 512, Some(2560))
        .expect("preview failure is non-fatal");
    assert_eq!(q.content_hash_hex, p.content_hash_hex);
    assert!(broken.has(&hex, 512));
    let _ = std::fs::remove_dir_all(&root);
}
