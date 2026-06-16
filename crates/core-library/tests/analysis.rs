//! Phase 2: AI-analysis persistence round-trip + incremental-skip + query-filter integration.

use core_db::Db;
use core_library::analysis::{
    analysis_facets, caption_for_image, detections_for_image, existing_analysis, insert_analysis,
    AnalysisInput,
};
use core_library::{query_images, QueryParams};
use serde_json::json;

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

fn records() -> Vec<AnalysisInput> {
    vec![
        AnalysisInput {
            analyzer_id: "object_detection".into(),
            model_version: "dfine-m-v1".into(),
            payload: json!({"detections": [
                {"label": "dog", "category": "Animals", "confidence": 0.91, "bbox": [1.0, 2.0, 3.0, 4.0]},
                {"label": "car", "category": "Vehicles", "confidence": 0.77, "bbox": [5.0, 6.0, 7.0, 8.0]}
            ]}),
        },
        AnalysisInput {
            analyzer_id: "caption".into(),
            model_version: "florence-v1".into(),
            payload: json!({"caption": "a dog next to a car", "keywords": ["dog", "car"]}),
        },
    ]
}

#[test]
fn persists_and_projects() {
    let mut db = Db::open_in_memory().unwrap();
    let img = seed_image(&db);
    let tx = db.conn.transaction().unwrap();
    insert_analysis(&tx, img, 100, &records()).unwrap();
    tx.commit().unwrap();

    let dets = detections_for_image(&db.conn, img).unwrap();
    assert_eq!(dets.len(), 2);
    assert_eq!(dets[0].label, "dog"); // ordered by confidence DESC
    assert_eq!(dets[0].bbox, [1.0, 2.0, 3.0, 4.0]);

    let cap = caption_for_image(&db.conn, img).unwrap().unwrap();
    assert_eq!(cap.caption, "a dog next to a car");
    assert_eq!(cap.keywords, vec!["dog", "car"]);

    let facets = analysis_facets(&db.conn).unwrap();
    assert!(facets
        .iter()
        .any(|f| f.category == "Animals" && f.count == 1));
    assert!(facets
        .iter()
        .any(|f| f.category == "Vehicles" && f.count == 1));
}

#[test]
fn query_filter_by_detected_category() {
    let mut db = Db::open_in_memory().unwrap();
    let img = seed_image(&db);
    let tx = db.conn.transaction().unwrap();
    insert_analysis(&tx, img, 100, &records()).unwrap();
    tx.commit().unwrap();

    let animals = query_images(
        &db.conn,
        &QueryParams {
            detected_category: Some("Animals".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(animals.len(), 1);

    let people = query_images(
        &db.conn,
        &QueryParams {
            detected_category: Some("People".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(people.is_empty());
}

#[test]
fn incremental_skip_set_and_idempotent_reinsert() {
    let mut db = Db::open_in_memory().unwrap();
    let img = seed_image(&db);

    let tx = db.conn.transaction().unwrap();
    insert_analysis(&tx, img, 100, &records()).unwrap();
    tx.commit().unwrap();

    let seen = existing_analysis(&db.conn).unwrap();
    assert!(seen.contains(&(img, "object_detection".into(), "dfine-m-v1".into())));
    assert!(seen.contains(&(img, "caption".into(), "florence-v1".into())));

    // Re-running the same analyzers is idempotent: no duplicate detection rows, single results row.
    let tx = db.conn.transaction().unwrap();
    insert_analysis(&tx, img, 200, &records()).unwrap();
    tx.commit().unwrap();
    assert_eq!(detections_for_image(&db.conn, img).unwrap().len(), 2);
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM analysis_results", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 2);
}
