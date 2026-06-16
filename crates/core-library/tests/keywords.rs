//! Keyword CRUD + image tagging over synthetic rows.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    add_keyword_to_image, create_or_get_keyword, delete_keyword, keywords_for_image, list_keywords,
    remove_keyword_from_image,
};

fn insert_img(conn: &Connection, tag: u8, filename: &str) -> i64 {
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
         VALUES (?1, 1, ?2, ?3, 'present', 0)",
        params![vec![tag; 32], format!("/lib/{filename}"), filename],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn create_or_get_dedups_case_insensitively() {
    let db = Db::open_in_memory().unwrap();
    let a = create_or_get_keyword(&db.conn, "Coast").unwrap();
    let b = create_or_get_keyword(&db.conn, "coast").unwrap();
    let c = create_or_get_keyword(&db.conn, "  COAST  ").unwrap();
    assert_eq!(a, b, "case-insensitive match");
    assert_eq!(a, c, "trimmed + case-insensitive match");
    assert_eq!(list_keywords(&db.conn).unwrap().len(), 1);
}

#[test]
fn empty_name_is_rejected() {
    let db = Db::open_in_memory().unwrap();
    assert!(create_or_get_keyword(&db.conn, "   ").is_err());
}

#[test]
fn tag_image_and_list_counts() {
    let db = Db::open_in_memory().unwrap();
    let i1 = insert_img(&db.conn, 1, "a.cr3");
    let i2 = insert_img(&db.conn, 2, "b.cr3");

    add_keyword_to_image(&db.conn, i1, "travel").unwrap();
    add_keyword_to_image(&db.conn, i2, "travel").unwrap();
    add_keyword_to_image(&db.conn, i1, "coast").unwrap();
    // Re-adding the same keyword to the same image is idempotent.
    add_keyword_to_image(&db.conn, i1, "coast").unwrap();

    let all = list_keywords(&db.conn).unwrap();
    // Ordered by name: coast, travel
    assert_eq!(all.len(), 2);
    let coast = all.iter().find(|k| k.name == "coast").unwrap();
    let travel = all.iter().find(|k| k.name == "travel").unwrap();
    assert_eq!(coast.count, 1);
    assert_eq!(travel.count, 2);

    let on_i1 = keywords_for_image(&db.conn, i1).unwrap();
    assert_eq!(on_i1.len(), 2, "i1 has coast + travel");
}

#[test]
fn remove_keyword_from_image_keeps_keyword() {
    let db = Db::open_in_memory().unwrap();
    let i1 = insert_img(&db.conn, 1, "a.cr3");
    let kw = add_keyword_to_image(&db.conn, i1, "sunset").unwrap();
    assert_eq!(keywords_for_image(&db.conn, i1).unwrap().len(), 1);

    remove_keyword_from_image(&db.conn, i1, kw.id).unwrap();
    assert!(keywords_for_image(&db.conn, i1).unwrap().is_empty());
    // Keyword still exists in the catalog (count 0).
    let all = list_keywords(&db.conn).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].count, 0);
}

#[test]
fn delete_keyword_cascades_links() {
    let db = Db::open_in_memory().unwrap();
    let i1 = insert_img(&db.conn, 1, "a.cr3");
    let kw = add_keyword_to_image(&db.conn, i1, "macro").unwrap();
    assert_eq!(keywords_for_image(&db.conn, i1).unwrap().len(), 1);

    delete_keyword(&db.conn, kw.id).unwrap();
    assert!(list_keywords(&db.conn).unwrap().is_empty());
    // FK ON DELETE CASCADE removed the image_keywords row too.
    assert!(keywords_for_image(&db.conn, i1).unwrap().is_empty());
    let link_count: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM image_keywords", [], |r| r.get(0))
        .unwrap();
    assert_eq!(link_count, 0);
}
