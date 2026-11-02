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

#[test]
fn rapid_saves_and_clock_regressions_get_distinct_cache_revisions() {
    let db = Db::open_in_memory().unwrap();
    let id = insert_image(&db.conn, &[0xcd; 32], "/tmp/edited.cr3");
    let time = 1_700_000_000;

    for (timestamp, expected_revision, json) in [
        (time, time, r#"{"exposure":1}"#),
        (time, time + 1, r#"{"exposure":2}"#),
        (time - 60, time + 2, r#"{"exposure":3}"#),
        (time + 100, time + 100, r#"{"exposure":4}"#),
    ] {
        core_library::set_edit(&db.conn, id, 5, json, timestamp).unwrap();
        let (stored_json, revision) = core_library::get_edit_with_version(&db.conn, id)
            .unwrap()
            .unwrap();
        assert_eq!(
            stored_json, json,
            "revision handling must not rewrite edits"
        );
        assert_eq!(revision, expected_revision);
        assert_eq!(
            core_library::thumb_precheck(&db.conn, id)
                .unwrap()
                .unwrap()
                .1,
            Some(expected_revision)
        );
        assert_eq!(
            db.conn
                .query_row(
                    "SELECT process_version FROM edits WHERE image_id = ?1",
                    [id],
                    |r| { r.get::<_, i64>(0) }
                )
                .unwrap(),
            5,
            "cache revision is separate from saved renderer provenance"
        );
    }
}
