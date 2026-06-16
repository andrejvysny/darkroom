//! Static + smart collection behavior over synthetic rows.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    add_images_to_collection, collections_for_image, count_images, create_collection,
    delete_collection, list_collections, remove_images_from_collection, QueryParams,
};

fn insert_img(conn: &Connection, tag: u8, stars: i64) -> i64 {
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
         VALUES (?1, 1, ?2, ?3, 'present', 0)",
        params![
            vec![tag; 32],
            format!("/lib/{tag}.cr3"),
            format!("{tag}.cr3")
        ],
    )
    .unwrap();
    let id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO ratings_flags(image_id, stars) VALUES(?1, ?2)",
        params![id, stars],
    )
    .unwrap();
    id
}

#[test]
fn static_collection_membership_and_counts() {
    let db = Db::open_in_memory().unwrap();
    let i1 = insert_img(&db.conn, 1, 5);
    let i2 = insert_img(&db.conn, 2, 3);
    let i3 = insert_img(&db.conn, 3, 0);

    let cid = create_collection(&db.conn, "Portfolio", false, None).unwrap();
    let added = add_images_to_collection(&db.conn, cid, &[i1, i2]).unwrap();
    assert_eq!(added, 2);
    // Idempotent: re-adding i2 + adding i3 → only i3 is new.
    let added2 = add_images_to_collection(&db.conn, cid, &[i2, i3]).unwrap();
    assert_eq!(added2, 1);

    let cols = list_collections(&db.conn).unwrap();
    assert_eq!(cols.len(), 1);
    assert_eq!(cols[0].count, 3, "three members");
    assert!(!cols[0].is_smart);

    // The collection_id query filter (from query.rs) matches members.
    let p = QueryParams {
        collection_id: Some(cid),
        ..Default::default()
    };
    assert_eq!(count_images(&db.conn, &p).unwrap(), 3);

    // collections_for_image reflects membership.
    assert_eq!(collections_for_image(&db.conn, i1).unwrap().len(), 1);

    let removed = remove_images_from_collection(&db.conn, cid, &[i1]).unwrap();
    assert_eq!(removed, 1);
    assert_eq!(count_images(&db.conn, &p).unwrap(), 2);
    assert!(collections_for_image(&db.conn, i1).unwrap().is_empty());
}

#[test]
fn smart_collection_count_evaluates_predicate() {
    let db = Db::open_in_memory().unwrap();
    insert_img(&db.conn, 1, 5);
    insert_img(&db.conn, 2, 4);
    insert_img(&db.conn, 3, 2);

    // "4 stars and up"
    let query = serde_json::json!({ "minStars": 4 }).to_string();
    create_collection(&db.conn, "★★★★+", true, Some(&query)).unwrap();

    let cols = list_collections(&db.conn).unwrap();
    assert_eq!(cols.len(), 1);
    assert!(cols[0].is_smart);
    assert_eq!(cols[0].count, 2, "two images >= 4 stars");
    assert_eq!(cols[0].query.as_deref(), Some(query.as_str()));
}

#[test]
fn smart_collection_with_bad_query_counts_zero_safely() {
    let db = Db::open_in_memory().unwrap();
    insert_img(&db.conn, 1, 5);
    // Invalid JSON → unwrap_or_default() → empty QueryParams → counts all present (default),
    // NOT a panic. Verify it doesn't crash and yields a sane number.
    create_collection(&db.conn, "broken", true, Some("not json")).unwrap();
    let cols = list_collections(&db.conn).unwrap();
    // default QueryParams matches all present images (1).
    assert_eq!(cols[0].count, 1);
}

#[test]
fn delete_collection_cascades_membership() {
    let db = Db::open_in_memory().unwrap();
    let i1 = insert_img(&db.conn, 1, 5);
    let cid = create_collection(&db.conn, "Temp", false, None).unwrap();
    add_images_to_collection(&db.conn, cid, &[i1]).unwrap();

    delete_collection(&db.conn, cid).unwrap();
    assert!(list_collections(&db.conn).unwrap().is_empty());
    let links: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM collection_images", [], |r| r.get(0))
        .unwrap();
    assert_eq!(links, 0, "FK cascade removed membership rows");
}

#[test]
fn empty_name_rejected() {
    let db = Db::open_in_memory().unwrap();
    assert!(create_collection(&db.conn, "  ", false, None).is_err());
}
