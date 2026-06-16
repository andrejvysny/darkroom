use core_db::rusqlite::params;
use core_db::Db;

fn insert(db: &Db, hash: &[u8], fp: Option<&[u8]>, path: &str) {
    db.conn
        .execute(
            "INSERT INTO images(content_hash, capture_fingerprint, file_size, path, original_filename, status, imported_at)
             VALUES(?1, ?2, ?3, ?4, ?5, 'present', 0)",
            params![hash, fp, 1000i64, path, path],
        )
        .unwrap();
}

#[test]
fn groups_byte_and_capture_then_resolve() {
    let db = Db::open_in_memory().unwrap();
    let (h1, h2, h3, h4) = ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]);
    let (fp_a, fp_b, fp_u) = ([9u8; 32], [8u8; 32], [7u8; 32]);

    // Byte-identical pair (same hash + same fingerprint).
    insert(&db, &h1, Some(&fp_a), "a1.cr3");
    insert(&db, &h1, Some(&fp_a), "a2.cr3");
    // Same-capture pair (different bytes, shared fingerprint).
    insert(&db, &h2, Some(&fp_b), "b1.cr3");
    insert(&db, &h3, Some(&fp_b), "b2.cr3");
    // Unique.
    insert(&db, &h4, Some(&fp_u), "u.cr3");

    let byte = core_dedup::find_byte_identical(&db.conn).unwrap();
    assert_eq!(byte.len(), 1, "one byte-identical group");
    assert_eq!(byte[0].images.len(), 2);
    assert_eq!(byte[0].category, "byte");

    let cap = core_dedup::find_same_capture(&db.conn).unwrap();
    assert_eq!(cap.len(), 2, "fp_a group + fp_b group");
    assert!(cap.iter().all(|g| g.images.len() == 2));

    // Resolve the byte-identical group: keep first, trash second. Paths don't exist on disk,
    // so the Trash step is skipped and only the catalog row is removed.
    let keep = byte[0].images[0].id;
    let drop = byte[0].images[1].id;
    let n = core_dedup::resolve(&db.conn, keep, &[drop]).unwrap();
    assert_eq!(n, 1);

    let remaining: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM images WHERE id=?1",
            params![drop],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0, "trashed row removed");
    let kept: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM images WHERE id=?1",
            params![keep],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, 1, "keeper preserved");

    // After resolve, no more byte-identical groups.
    assert_eq!(core_dedup::find_byte_identical(&db.conn).unwrap().len(), 0);
}
