//! "Remove from library" drops catalog rows only; files and sidecars stay on disk.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    add_images_to_collection, add_keyword_to_images, create_collection, link_pair,
    remove_from_catalog, set_edit,
};
use std::path::{Path, PathBuf};

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("darkroom-remove-lib-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Row + real file + `.json` sidecar beside it.
fn insert_file(conn: &Connection, dir: &Path, tag: u8, filename: &str) -> i64 {
    let path = dir.join(filename);
    std::fs::write(&path, vec![tag; 16]).unwrap();
    std::fs::write(path.with_extension("json"), b"{}").unwrap();
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
         VALUES (?1, 16, ?2, ?3, 'present', 0)",
        params![vec![tag; 32], path.display().to_string(), filename],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn removes_rows_and_cascades_but_keeps_files_and_sidecars() {
    let dir = tempdir("basic");
    let db = Db::open_in_memory().unwrap();
    let a = insert_file(&db.conn, &dir, 1, "a.cr3");
    let b = insert_file(&db.conn, &dir, 2, "b.cr3");
    set_edit(&db.conn, a, 1, "{}", 5).unwrap();
    add_keyword_to_images(&db.conn, &[a, b], "trip").unwrap();
    let coll = create_collection(&db.conn, "c", false, None).unwrap();
    add_images_to_collection(&db.conn, coll, &[a, b]).unwrap();

    let out = remove_from_catalog(&db.conn, &[a]).unwrap();
    assert_eq!((out.removed, out.companions), (1, 0));
    assert_eq!(count(&db.conn, "SELECT count(*) FROM images"), 1);
    assert_eq!(count(&db.conn, "SELECT count(*) FROM edits"), 0);
    assert_eq!(count(&db.conn, "SELECT count(*) FROM image_keywords"), 1);
    assert_eq!(count(&db.conn, "SELECT count(*) FROM collection_images"), 1);
    assert!(dir.join("a.cr3").exists() && dir.join("a.json").exists());
}

#[test]
fn companion_goes_with_its_raw_and_is_counted() {
    let dir = tempdir("pair");
    let db = Db::open_in_memory().unwrap();
    let raw = insert_file(&db.conn, &dir, 1, "x.cr3");
    let jpg = insert_file(&db.conn, &dir, 2, "x.jpg");
    let other = insert_file(&db.conn, &dir, 3, "y.cr3");
    assert!(link_pair(&db.conn, raw, jpg).unwrap());

    let out = remove_from_catalog(&db.conn, &[raw, raw]).unwrap();
    assert_eq!((out.removed, out.companions), (2, 1));
    assert_eq!(count(&db.conn, "SELECT count(*) FROM images"), 1);
    assert_eq!(count(&db.conn, "SELECT count(*) FROM image_pairs"), 0);
    assert!(dir.join("x.jpg").exists() && dir.join("x.cr3").exists());
    assert!(other > 0);
}

#[test]
fn unknown_ids_are_ignored() {
    let dir = tempdir("unknown");
    let db = Db::open_in_memory().unwrap();
    let a = insert_file(&db.conn, &dir, 1, "a.cr3");
    let out = remove_from_catalog(&db.conn, &[9999]).unwrap();
    assert_eq!((out.removed, out.companions), (0, 0));
    assert_eq!(count(&db.conn, "SELECT count(*) FROM images"), 1);
    assert!(a > 0);
}
