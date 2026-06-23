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

fn insert_sim(db: &Db, hash: &[u8], path: &str, captured: i64, dhash: u64, phash: u64) -> i64 {
    db.conn
        .execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                                capture_date, imported_at, width, height, camera_model, body_serial)
             VALUES(?1, ?2, ?3, ?4, 'present', ?5, 0, 6000, 4000, 'Canon', 'body-1')",
            params![hash, 1000i64, path, path, captured],
        )
        .unwrap();
    let id = db.conn.last_insert_rowid();
    insert_features(db, id, dhash, phash, &gradient_luma(0), &landscape_color());
    id
}

fn insert_features(db: &Db, id: i64, dhash: u64, phash: u64, luma: &[u8], color: &[u8]) {
    db.conn
        .execute(
            "INSERT INTO image_similarity_features(
                image_id, feature_version, dhash64, phash64, tiny_luma32, color_grid4x4, computed_at
             ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, 0)",
            params![
                id,
                core_dedup::SIMILARITY_FEATURE_VERSION,
                dhash as i64,
                phash as i64,
                luma,
                color,
            ],
        )
        .unwrap();
}

fn gradient_luma(offset: u8) -> Vec<u8> {
    (0..1024)
        .map(|i| ((i % 32) as u8).saturating_mul(4).saturating_add(offset))
        .collect()
}

fn flat_luma(value: u8) -> Vec<u8> {
    vec![value; 1024]
}

fn landscape_color() -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    for y in 0..4 {
        for _ in 0..4 {
            out.extend(if y < 2 { [90, 150, 220] } else { [90, 130, 60] });
        }
    }
    out
}

fn sky_color() -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    for _ in 0..16 {
        out.extend([130, 180, 230]);
    }
    out
}

#[test]
fn perceptual_groups_within_threshold() {
    let db = Db::open_in_memory().unwrap();
    let base: u64 = 0x00FF_00FF_00FF_00FF;
    insert_sim(&db, &[10u8; 32], "a.cr3", 100, base, base);
    insert_sim(&db, &[11u8; 32], "b.cr3", 101, base ^ 0b1, base ^ 0b1);
    insert_sim(&db, &[12u8; 32], "c.cr3", 102, base ^ 0b11, base ^ 0b11);
    let d = insert_sim(&db, &[13u8; 32], "d.cr3", 101, !base, !base);
    db.conn
        .execute(
            "UPDATE image_similarity_features SET tiny_luma32=?1, color_grid4x4=?2 WHERE image_id=?3",
            params![flat_luma(20), sky_color(), d],
        )
        .unwrap();

    let g = core_dedup::find_perceptual(&db.conn, 4).unwrap();
    assert_eq!(g.len(), 1, "the three near hashes form one group");
    assert_eq!(g[0].images.len(), 3);
    assert_eq!(g[0].category, "perceptual");
    assert!(
        !g[0].images.iter().any(|i| i.filename == "d.cr3"),
        "the far hash is excluded"
    );
}

#[test]
fn perceptual_requires_capture_time_window() {
    let db = Db::open_in_memory().unwrap();
    let base = 0x00FF_00FF_00FF_00FFu64;
    insert_sim(&db, &[20u8; 32], "near.cr3", 100, base, base);
    insert_sim(&db, &[21u8; 32], "late.cr3", 200, base ^ 1, base ^ 1);

    assert_eq!(core_dedup::find_perceptual(&db.conn, 10).unwrap().len(), 0);
    assert_eq!(core_dedup::find_perceptual(&db.conn, 11).unwrap().len(), 1);
}

#[test]
fn perceptual_rejects_same_hash_but_different_scene_color() {
    let db = Db::open_in_memory().unwrap();
    let base = 0x00FF_00FF_00FF_00FFu64;
    let a = insert_sim(&db, &[30u8; 32], "field.cr3", 100, base, base);
    let b = insert_sim(&db, &[31u8; 32], "sky.cr3", 101, base ^ 1, base ^ 1);
    db.conn
        .execute(
            "UPDATE image_similarity_features SET tiny_luma32=?1, color_grid4x4=?2 WHERE image_id=?3",
            params![gradient_luma(0), landscape_color(), a],
        )
        .unwrap();
    db.conn
        .execute(
            "UPDATE image_similarity_features SET tiny_luma32=?1, color_grid4x4=?2 WHERE image_id=?3",
            params![flat_luma(160), sky_color(), b],
        )
        .unwrap();

    assert_eq!(core_dedup::find_perceptual(&db.conn, 10).unwrap().len(), 0);
}

#[test]
fn perceptual_splits_weak_transitive_chains() {
    let db = Db::open_in_memory().unwrap();
    let a = 0x0000_0000_0000_00FFu64;
    let b = 0x0000_0000_0000_0000u64;
    let c = 0x0000_0000_0000_FF00u64;
    insert_sim(&db, &[40u8; 32], "a.cr3", 100, a, a);
    insert_sim(&db, &[41u8; 32], "b.cr3", 101, b, b);
    insert_sim(&db, &[42u8; 32], "c.cr3", 102, c, c);

    let groups = core_dedup::find_perceptual(&db.conn, 10).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].images.len(), 2, "do not form A-B-C via bridge B");
}

#[test]
fn perceptual_ignores_missing_capture_date() {
    let db = Db::open_in_memory().unwrap();
    let base = 0x00FF_00FF_00FF_00FFu64;
    insert_sim(&db, &[50u8; 32], "a.cr3", 100, base, base);
    let id = insert_sim(&db, &[51u8; 32], "b.cr3", 101, base ^ 1, base ^ 1);
    db.conn
        .execute(
            "UPDATE images SET capture_date=NULL WHERE id=?1",
            params![id],
        )
        .unwrap();

    assert_eq!(core_dedup::find_perceptual(&db.conn, 10).unwrap().len(), 0);
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
