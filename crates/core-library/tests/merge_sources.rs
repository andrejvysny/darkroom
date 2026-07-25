//! `merge_sources` — the "Source frames" panel query. Exercised against synthetic rows inserted
//! directly via SQL (no RAW decode / actual merge needed): an HDR bracket (`hdr_sources`, EV
//! offsets), a panorama stitch (`panorama_sources`, no EV), and a plain un-merged image (`None`).

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::merge_sources;

/// Insert a minimal image row; returns its id. `tag` makes the content_hash unique. `present`
/// controls `status` ("present" vs "missing") so tests can cover a relinked-away source frame.
fn insert_img(conn: &Connection, tag: u8, filename: &str, present: bool) -> i64 {
    let hash = vec![tag; 32];
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status, imported_at)
         VALUES (?1, 1024, ?2, ?3, ?4, 0)",
        params![
            hash,
            format!("/lib/{filename}"),
            filename,
            if present { "present" } else { "missing" },
        ],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[test]
fn hdr_row_returns_relative_ev_and_present_flag() {
    let db = Db::open_in_memory().unwrap();
    let c = &db.conn;
    let hdr = insert_img(c, 1, "merged_HDR.exr", true);
    let under = insert_img(c, 2, "under.cr3", true);
    let reference = insert_img(c, 3, "reference.cr3", true);
    let over = insert_img(c, 4, "over.cr3", false); // relinked away since the merge

    c.execute(
        "INSERT INTO hdr_sources(hdr_image_id, source_image_id, position, relative_ev)
         VALUES (?1,?2,0,-2.0), (?1,?3,1,0.0), (?1,?4,2,2.0)",
        params![hdr, under, reference, over],
    )
    .unwrap();

    let result = merge_sources(c, hdr)
        .unwrap()
        .expect("hdr row must resolve");
    assert_eq!(result.kind, "hdr");
    assert_eq!(result.sources.len(), 3);

    // Ordered by position.
    assert_eq!(result.sources[0].image_id, under);
    assert_eq!(result.sources[0].filename, "under.cr3");
    assert_eq!(result.sources[0].relative_ev, Some(-2.0));
    assert!(result.sources[0].present);

    assert_eq!(result.sources[1].image_id, reference);
    assert_eq!(result.sources[1].relative_ev, Some(0.0));
    assert!(result.sources[1].present);

    assert_eq!(result.sources[2].image_id, over);
    assert_eq!(result.sources[2].relative_ev, Some(2.0));
    assert!(
        !result.sources[2].present,
        "a relinked-away (status='missing') source must report present=false, not disappear"
    );
}

#[test]
fn panorama_row_has_no_relative_ev() {
    let db = Db::open_in_memory().unwrap();
    let c = &db.conn;
    let pano = insert_img(c, 1, "stitched.dng", true);
    let left = insert_img(c, 2, "left.cr3", true);
    let right = insert_img(c, 3, "right.cr3", true);

    c.execute(
        "INSERT INTO panorama_sources(pano_image_id, source_image_id, position)
         VALUES (?1,?2,0), (?1,?3,1)",
        params![pano, left, right],
    )
    .unwrap();

    let result = merge_sources(c, pano)
        .unwrap()
        .expect("panorama row must resolve");
    assert_eq!(result.kind, "panorama");
    assert_eq!(result.sources.len(), 2);
    assert_eq!(result.sources[0].image_id, left);
    assert_eq!(result.sources[1].image_id, right);
    assert!(
        result.sources.iter().all(|s| s.relative_ev.is_none()),
        "panorama sources carry no EV offset"
    );
}

#[test]
fn plain_image_returns_none() {
    let db = Db::open_in_memory().unwrap();
    let c = &db.conn;
    let plain = insert_img(c, 1, "IMG_0001.CR3", true);
    assert!(merge_sources(c, plain).unwrap().is_none());
    // Also a non-existent id — same contract, no error.
    assert!(merge_sources(c, 999).unwrap().is_none());
}
