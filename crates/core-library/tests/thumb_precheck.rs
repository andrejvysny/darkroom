//! The thumbnail backfill's pre-check runs for every present image at startup, so it reads the
//! content hash and the edit version in ONE query. This pins it to the two lookups it replaced.

use core_db::rusqlite::{params, Connection};
use core_db::Db;

// `images.content_hash` holds the 32 raw BLAKE3 bytes; readers hex-encode them.
fn insert_image(conn: &Connection, hash: &[u8; 32], path: &str) -> i64 {
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                            capture_date, imported_at)
         VALUES (?1, 64, ?2, ?3, 'present', 1000, 0)",
        params![hash.to_vec(), path, "shot.cr3"],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn reports_the_hash_and_the_edit_version() {
    let db = Db::open_in_memory().unwrap();
    let id = insert_image(&db.conn, &[0xab; 32], "/tmp/shot.cr3");

    let (hash, edit_version) = core_library::thumb_precheck(&db.conn, id)
        .unwrap()
        .expect("row exists");
    assert_eq!(
        hash,
        "ab".repeat(32),
        "hex of the stored bytes, as the thumb cache keys expect"
    );
    assert_eq!(edit_version, None, "an unedited image has no edit version");

    core_library::set_edit(&db.conn, id, 5, "{}", 1_700_000_000).unwrap();
    let (_, edit_version) = core_library::thumb_precheck(&db.conn, id)
        .unwrap()
        .expect("row exists");
    assert_eq!(
        edit_version,
        core_library::get_edit_with_version(&db.conn, id)
            .unwrap()
            .map(|(_, v)| v),
        "must agree with the lookup it replaced",
    );
}

#[test]
fn a_missing_row_is_none_not_an_error() {
    let db = Db::open_in_memory().unwrap();
    assert!(core_library::thumb_precheck(&db.conn, 99_999)
        .unwrap()
        .is_none());
}
