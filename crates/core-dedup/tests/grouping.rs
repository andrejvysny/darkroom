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
    let res = core_dedup::resolve(&db.conn, keep, &[drop]).unwrap();
    assert_eq!(res.trashed, 1);
    assert_eq!(res.trashed_hashes.len(), 1, "one trashed hash reported");

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

#[test]
fn resolve_rejects_invalid_keeper_and_trashes_nothing() {
    let db = Db::open_in_memory().unwrap();
    let h1 = [1u8; 32];
    insert(&db, &h1, None, "a1.cr3");
    insert(&db, &h1, None, "a2.cr3");

    let group = core_dedup::find_byte_identical(&db.conn).unwrap();
    let a1 = group[0].images[0].id;
    let a2 = group[0].images[1].id;

    // A keeper id that does not exist must abort with no deletions.
    let stale_keeper = a1.max(a2) + 999;
    let err = core_dedup::resolve(&db.conn, stale_keeper, &[a1, a2]);
    assert!(
        matches!(err, Err(core_dedup::DedupError::InvalidKeeper(_))),
        "stale keeper must error"
    );
    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "nothing trashed when the keeper is invalid");

    // A keeper that was already trashed (row gone) is likewise rejected.
    db.conn
        .execute("DELETE FROM images WHERE id=?1", params![a1])
        .unwrap();
    let err = core_dedup::resolve(&db.conn, a1, &[a2]);
    assert!(
        matches!(err, Err(core_dedup::DedupError::InvalidKeeper(_))),
        "already-resolved keeper must error"
    );
    let kept: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM images WHERE id=?1",
            params![a2],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        kept, 1,
        "the last copy is preserved when the keeper is stale"
    );
}

fn insert_ph(db: &Db, hash: &[u8], path: &str, phash: u64) {
    db.conn
        .execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at, phash)
             VALUES(?1, ?2, ?3, ?4, 'present', 0, ?5)",
            params![hash, 1000i64, path, path, phash as i64],
        )
        .unwrap();
}

#[test]
fn perceptual_groups_within_threshold() {
    let db = Db::open_in_memory().unwrap();
    let base: u64 = 0x00FF_00FF_00FF_00FF;
    insert_ph(&db, &[10u8; 32], "a.cr3", base);
    insert_ph(&db, &[11u8; 32], "b.cr3", base ^ 0b1); // 1 bit from base
    insert_ph(&db, &[12u8; 32], "c.cr3", base ^ 0b11); // 2 bits from base
    insert_ph(&db, &[13u8; 32], "d.cr3", !base); // ~64 bits away

    let g = core_dedup::find_perceptual(&db.conn, 4).unwrap();
    assert_eq!(g.len(), 1, "the three near hashes form one group");
    assert_eq!(g[0].images.len(), 3);
    assert_eq!(g[0].category, "perceptual");
    assert!(
        !g[0].images.iter().any(|i| i.filename == "d.cr3"),
        "the far hash is excluded"
    );

    // Threshold 0: only exact-phash matches group — here none, so no groups.
    assert_eq!(core_dedup::find_perceptual(&db.conn, 0).unwrap().len(), 0);
}

#[test]
fn auto_resolve_keeps_one_per_byte_group() {
    let db = Db::open_in_memory().unwrap();
    let (h1, h2) = ([1u8; 32], [2u8; 32]);
    // Group 1: three identical; Group 2: two identical.
    insert(&db, &h1, None, "g1a.cr3");
    insert(&db, &h1, None, "g1b.cr3");
    insert(&db, &h1, None, "g1c.cr3");
    insert(&db, &h2, None, "g2a.cr3");
    insert(&db, &h2, None, "g2b.cr3");

    let res = core_dedup::auto_resolve_byte_identical(&db.conn).unwrap();
    assert_eq!(res.trashed, 3, "2 + 1 duplicates trashed");

    let count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM images", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2, "one keeper per group survives");
    assert_eq!(core_dedup::find_byte_identical(&db.conn).unwrap().len(), 0);
}
