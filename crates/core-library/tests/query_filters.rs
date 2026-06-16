//! Catalog query filtering + sorting, exercised against synthetic rows inserted directly via SQL
//! (no RAW decode needed). Covers stars/flag/color-label filters, keyword & collection membership,
//! keyword-name search, and every sort order.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{count_images, query_images, QueryParams};

/// Insert a minimal image row; returns its id. `tag` makes the content_hash unique.
fn insert_img(
    conn: &Connection,
    tag: u8,
    filename: &str,
    capture_date: i64,
    camera: &str,
    imported_at: i64,
) -> i64 {
    let hash = vec![tag; 32];
    conn.execute(
        "INSERT INTO images(
            content_hash, file_size, path, original_filename, status,
            capture_date, camera_model, imported_at
         ) VALUES (?1,?2,?3,?4,'present',?5,?6,?7)",
        params![
            hash,
            1024_i64,
            format!("/lib/{filename}"),
            filename,
            capture_date,
            camera,
            imported_at,
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn set_rf(conn: &Connection, image_id: i64, stars: i64, flag: &str, label: Option<&str>) {
    conn.execute(
        "INSERT INTO ratings_flags(image_id, stars, flag, color_label) VALUES(?1,?2,?3,?4)",
        params![image_id, stars, flag, label],
    )
    .unwrap();
}

fn add_keyword(conn: &Connection, name: &str) -> i64 {
    conn.execute("INSERT INTO keywords(name) VALUES(?1)", params![name])
        .unwrap();
    conn.last_insert_rowid()
}

fn tag_image(conn: &Connection, image_id: i64, keyword_id: i64) {
    conn.execute(
        "INSERT INTO image_keywords(image_id, keyword_id) VALUES(?1,?2)",
        params![image_id, keyword_id],
    )
    .unwrap();
}

/// Build a small fixed catalog. Returns the connection (owned by the in-memory Db).
fn seed() -> Db {
    let db = Db::open_in_memory().unwrap();
    let c = &db.conn;
    // id 1: 5★ pick red, "Canon",   capture 300, imported 10
    // id 2: 3★ none green, "Canon", capture 200, imported 20
    // id 3: 0★ reject (no label), "Nikon", capture 100, imported 30
    // id 4: 1★ pick (no label), "Nikon", capture 400, imported 5
    let a = insert_img(c, 1, "AAA.CR3", 300, "Canon R7", 10);
    let b = insert_img(c, 2, "BBB.CR3", 200, "Canon R7", 20);
    let d = insert_img(c, 3, "CCC.NEF", 100, "Nikon Z6", 30);
    let e = insert_img(c, 4, "DDD.NEF", 400, "Nikon Z6", 5);
    set_rf(c, a, 5, "pick", Some("red"));
    set_rf(c, b, 3, "none", Some("green"));
    set_rf(c, d, 0, "reject", None);
    set_rf(c, e, 1, "pick", None);
    db
}

fn ids(conn: &Connection, p: &QueryParams) -> Vec<i64> {
    query_images(conn, p).unwrap().into_iter().map(|r| r.id).collect()
}

#[test]
fn min_stars_filter() {
    let db = seed();
    let p = QueryParams {
        min_stars: Some(3),
        ..Default::default()
    };
    let got = ids(&db.conn, &p);
    assert_eq!(got.len(), 2, "two images have >= 3 stars");
    assert_eq!(count_images(&db.conn, &p).unwrap(), 2);
}

#[test]
fn flag_filter() {
    let db = seed();
    let picks = QueryParams {
        flag: Some("pick".into()),
        ..Default::default()
    };
    assert_eq!(count_images(&db.conn, &picks).unwrap(), 2);
    let rejects = QueryParams {
        flag: Some("reject".into()),
        ..Default::default()
    };
    assert_eq!(count_images(&db.conn, &rejects).unwrap(), 1);
    let none = QueryParams {
        flag: Some("none".into()),
        ..Default::default()
    };
    assert_eq!(count_images(&db.conn, &none).unwrap(), 1);
}

#[test]
fn color_label_filter_and_unlabeled_sentinel() {
    let db = seed();
    let red = QueryParams {
        color_label: Some("red".into()),
        ..Default::default()
    };
    assert_eq!(count_images(&db.conn, &red).unwrap(), 1);

    // Sentinel "__none__" matches rows with NULL color_label.
    let unlabeled = QueryParams {
        color_label: Some("__none__".into()),
        ..Default::default()
    };
    assert_eq!(count_images(&db.conn, &unlabeled).unwrap(), 2);
}

#[test]
fn keyword_membership_filter() {
    let db = seed();
    let coast = add_keyword(&db.conn, "coast");
    tag_image(&db.conn, 1, coast);
    tag_image(&db.conn, 3, coast);

    let p = QueryParams {
        keyword_id: Some(coast),
        ..Default::default()
    };
    let mut got = ids(&db.conn, &p);
    got.sort();
    assert_eq!(got, vec![1, 3]);
    assert_eq!(count_images(&db.conn, &p).unwrap(), 2);
}

#[test]
fn collection_membership_filter() {
    let db = seed();
    db.conn
        .execute("INSERT INTO collections(name) VALUES('Portfolio')", [])
        .unwrap();
    let cid = db.conn.last_insert_rowid();
    db.conn
        .execute(
            "INSERT INTO collection_images(collection_id, image_id) VALUES(?1, 2),(?1, 4)",
            params![cid],
        )
        .unwrap();

    let p = QueryParams {
        collection_id: Some(cid),
        ..Default::default()
    };
    let mut got = ids(&db.conn, &p);
    got.sort();
    assert_eq!(got, vec![2, 4]);
}

#[test]
fn search_matches_filename_camera_and_keyword() {
    let db = seed();
    let travel = add_keyword(&db.conn, "travel");
    tag_image(&db.conn, 4, travel);

    // filename
    assert_eq!(
        count_images(
            &db.conn,
            &QueryParams {
                search: Some("AAA".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        1
    );
    // camera substring
    assert_eq!(
        count_images(
            &db.conn,
            &QueryParams {
                search: Some("Nikon".into()),
                ..Default::default()
            }
        )
        .unwrap(),
        2
    );
    // keyword name
    let by_kw = QueryParams {
        search: Some("travel".into()),
        ..Default::default()
    };
    assert_eq!(ids(&db.conn, &by_kw), vec![4]);
}

#[test]
fn sort_orders() {
    let db = seed();
    let by = |s: &str| {
        ids(
            &db.conn,
            &QueryParams {
                sort: Some(s.into()),
                ..Default::default()
            },
        )
    };
    // capture dates: id4=400, id1=300, id2=200, id3=100
    assert_eq!(by("capture_desc"), vec![4, 1, 2, 3]);
    assert_eq!(by("capture_asc"), vec![3, 2, 1, 4]);
    // filenames: AAA, BBB, CCC, DDD → ids 1,2,3,4
    assert_eq!(by("filename"), vec![1, 2, 3, 4]);
    assert_eq!(by("filename_desc"), vec![4, 3, 2, 1]);
    // stars: id1=5, id2=3, id4=1, id3=0
    assert_eq!(by("rating_desc"), vec![1, 2, 4, 3]);
    assert_eq!(by("rating_asc"), vec![3, 4, 2, 1]);
    // imported_at: id3=30, id2=20, id1=10, id4=5
    assert_eq!(by("imported_desc"), vec![3, 2, 1, 4]);
    assert_eq!(by("imported_asc"), vec![4, 1, 2, 3]);
    // unknown sort falls back to capture_desc
    assert_eq!(by("bogus"), vec![4, 1, 2, 3]);
}

#[test]
fn combined_filters_and_limit_offset() {
    let db = seed();
    // picks with >= 1 star, sorted by capture desc → id1(300), id4(400) → [4,1]
    let p = QueryParams {
        flag: Some("pick".into()),
        min_stars: Some(1),
        sort: Some("capture_desc".into()),
        ..Default::default()
    };
    assert_eq!(ids(&db.conn, &p), vec![4, 1]);

    // limit/offset paginate
    let page = QueryParams {
        sort: Some("filename".into()),
        limit: Some(2),
        offset: Some(1),
        ..Default::default()
    };
    assert_eq!(ids(&db.conn, &page), vec![2, 3]);
}

#[test]
fn empty_params_returns_all() {
    let db = seed();
    assert_eq!(count_images(&db.conn, &QueryParams::default()).unwrap(), 4);
}
