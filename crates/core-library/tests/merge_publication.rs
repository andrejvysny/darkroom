use core_db::{rusqlite::params, Db};
use core_library::{add_root, insert_hdr_image, merge_sources, ProcessedImage};
use core_raw::{HdrSourceInfo, RawMeta};
use std::path::Path;

fn processed() -> ProcessedImage {
    ProcessedImage {
        content_hash: [42; 32],
        content_hash_hex: "2a".repeat(32),
        file_size: 100,
        path: "/library/merged.exr".into(),
        original_filename: "merged.exr".into(),
        meta: RawMeta::default(),
        width: 2,
        height: 2,
        capture_fingerprint: None,
        format: "hdr",
    }
}

fn source(db: &Db) -> HdrSourceInfo {
    db.conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
         VALUES (?1, 100, '/library/source.dng', 'source.dng', 'present', 0)",
        params![vec![1u8; 32]],
    ).unwrap();
    HdrSourceInfo {
        image_id: db.conn.last_insert_rowid(),
        content_hash_hex: "01".repeat(32),
        relative_ev: 0.0,
    }
}

#[test]
fn failed_source_link_rolls_back_image_and_prior_links() {
    let db = Db::open_in_memory().unwrap();
    let folder = add_root(&db.conn, Path::new("/library")).unwrap();
    let valid = source(&db);
    let missing = HdrSourceInfo {
        image_id: 999,
        content_hash_hex: "ff".repeat(32),
        relative_ev: 1.0,
    };
    assert!(insert_hdr_image(&db.conn, folder, 0, &processed(), &[valid, missing]).is_err());
    let images: i64 = db
        .conn
        .query_row("SELECT count(*) FROM images", [], |r| r.get(0))
        .unwrap();
    let links: i64 = db
        .conn
        .query_row("SELECT count(*) FROM hdr_sources", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        images, 1,
        "failed merge must not leave a present row for the removed file"
    );
    assert_eq!(links, 0, "the first source link must roll back too");
}

#[test]
fn successful_publication_is_complete_and_duplicate_preserves_provenance() {
    let db = Db::open_in_memory().unwrap();
    let folder = add_root(&db.conn, Path::new("/library")).unwrap();
    let sources = [source(&db)];
    let id = insert_hdr_image(&db.conn, folder, 0, &processed(), &sources)
        .unwrap()
        .unwrap();
    let links = merge_sources(&db.conn, id).unwrap().unwrap();
    assert_eq!(links.sources.len(), 1);
    assert_eq!(links.sources[0].image_id, sources[0].image_id);
    assert!(insert_hdr_image(&db.conn, folder, 1, &processed(), &[])
        .unwrap()
        .is_none());
    assert_eq!(
        merge_sources(&db.conn, id).unwrap().unwrap().sources.len(),
        1
    );
}
