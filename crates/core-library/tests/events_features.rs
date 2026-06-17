//! Behavioral-capture round-trip: append events + compute/store image features, then read back.

use core_db::Db;
use core_library::{
    append_event, compute_features, event_count, has_features, ids_json, images_missing_features,
    set_image_features, Event,
};
use core_raw::LinearImage;

fn seed_image(db: &Db) -> i64 {
    db.conn
        .execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, imported_at)
             VALUES (x'00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff', 1, 'p.cr3', 'p.cr3', 0)",
            [],
        )
        .unwrap();
    db.conn.last_insert_rowid()
}

#[test]
fn events_append_and_query() {
    let db = Db::open_in_memory().unwrap();
    let img = seed_image(&db);
    let base = || Event {
        session_id: "s1".into(),
        app_version: "test".into(),
        ..Default::default()
    };

    // A develop commit, a within-group pick + reject, and a dedup keeper decision.
    append_event(
        &db.conn,
        &Event {
            event_type: "develop.params_commit".into(),
            image_id: Some(img),
            params_after: Some("{\"exposure\":0.5}".into()),
            touch_count: Some(3),
            ..base()
        },
    )
    .unwrap();
    append_event(
        &db.conn,
        &Event {
            event_type: "culling.flag_pick".into(),
            image_id: Some(img),
            chosen_id: Some(img),
            flag: Some("pick".into()),
            group_id: Some("g1".into()),
            candidate_ids: Some(ids_json(&[img, 2, 3])),
            latency_ms: Some(1200),
            ..base()
        },
    )
    .unwrap();
    append_event(
        &db.conn,
        &Event {
            event_type: "dedup.keeper_chosen".into(),
            group_id: Some("g1".into()),
            chosen_id: Some(img),
            rejected_ids: Some(ids_json(&[2, 3])),
            suggestion_id: Some(2),
            ..base()
        },
    )
    .unwrap();

    assert_eq!(event_count(&db.conn).unwrap(), 3);

    // The keeper decision round-trips with its candidate context.
    let (chosen, rejected): (i64, String) = db
        .conn
        .query_row(
            "SELECT chosen_id, rejected_ids FROM user_events WHERE event_type='dedup.keeper_chosen'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(chosen, img);
    assert_eq!(rejected, "[2,3]");
}

#[test]
fn features_compute_and_persist() {
    let db = Db::open_in_memory().unwrap();
    let img = seed_image(&db);

    // Synthetic 32×24 linear image with a horizontal luma gradient + mild chroma.
    let (w, h) = (32u32, 24u32);
    let mut data = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = x as f32 / (w - 1) as f32;
            data.push(t); // R
            data.push(t * 0.9 + 0.05); // G
            data.push(t * 0.8); // B
            let _ = y;
        }
    }
    let lin = LinearImage {
        width: w,
        height: h,
        data,
    };
    let f = compute_features(&lin, [2.0, 1.0, 1.5, 1.0]);

    assert!((f.wb_as_shot_rg - 2.0).abs() < 1e-6);
    assert!((f.wb_as_shot_bg - 1.5).abs() < 1e-6);
    assert_eq!(f.hist_luma.len(), 256);
    assert_eq!(f.hist_logchroma.len(), 32 * 32);
    assert!((f.hist_luma.iter().sum::<f32>() - 1.0).abs() < 1e-3); // normalized
    assert!(f.sharpness >= 0.0);
    assert!(f.dynamic_range_ev > 0.0);

    assert_eq!(images_missing_features(&db.conn).unwrap().len(), 1);
    set_image_features(&db.conn, img, &f, 123).unwrap();
    assert!(has_features(&db.conn, img).unwrap());
    assert!(images_missing_features(&db.conn).unwrap().is_empty());
}
