//! Batch culling + batch keyword tagging over synthetic rows.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    add_keyword_to_images, keywords_for_image, set_flag_many, set_label_many, set_rating_many,
    QueryParams,
};

fn insert_img(conn: &Connection, tag: u8) -> i64 {
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
    conn.last_insert_rowid()
}

fn stars_of(conn: &Connection, id: i64) -> i64 {
    conn.query_row(
        "SELECT COALESCE(stars,0) FROM ratings_flags WHERE image_id = ?1",
        [id],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

#[test]
fn batch_ops() {
    let mut db = Db::open_in_memory().unwrap();
    let ids: Vec<i64> = (1..=3).map(|t| insert_img(&db.conn, t)).collect();

    // Batch rating (clamped).
    set_rating_many(&mut db.conn, &ids, 9).unwrap();
    for &id in &ids {
        assert_eq!(stars_of(&db.conn, id), 5, "clamped to 5");
    }

    // Batch flag.
    set_flag_many(&mut db.conn, &ids, "pick").unwrap();
    use core_library::count_images;
    assert_eq!(
        count_images(
            &db.conn,
            &QueryParams {
                flag: Some("pick".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        3
    );

    // Batch label, then clear on a subset.
    set_label_many(&mut db.conn, &ids, Some("red")).unwrap();
    assert_eq!(
        count_images(
            &db.conn,
            &QueryParams {
                color_label: Some("red".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        3
    );
    set_label_many(&mut db.conn, &ids[..1], None).unwrap();
    assert_eq!(
        count_images(
            &db.conn,
            &QueryParams {
                color_label: Some("red".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        2
    );

    // Invalid flag normalizes to "none".
    set_flag_many(&mut db.conn, &ids, "bogus").unwrap();
    assert_eq!(
        count_images(
            &db.conn,
            &QueryParams {
                flag: Some("none".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        3
    );
}

#[test]
fn batch_keyword_tags_all_images() {
    let db = Db::open_in_memory().unwrap();
    let ids: Vec<i64> = (1..=3).map(|t| insert_img(&db.conn, t)).collect();

    let kw = add_keyword_to_images(&db.conn, &ids, "trip").unwrap();
    assert_eq!(kw.name, "trip");
    for &id in &ids {
        let on = keywords_for_image(&db.conn, id).unwrap();
        assert_eq!(on.len(), 1);
        assert_eq!(on[0].name, "trip");
    }
    // Idempotent re-tag.
    add_keyword_to_images(&db.conn, &ids, "trip").unwrap();
    assert_eq!(keywords_for_image(&db.conn, ids[0]).unwrap().len(), 1);
}
