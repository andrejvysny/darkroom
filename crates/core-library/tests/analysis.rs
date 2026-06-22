//! Phase 2: AI-analysis persistence round-trip + incremental-skip + query-filter integration.

use core_db::rusqlite::params;
use core_db::Db;
use core_library::analysis::{
    analysis_facets, caption_for_image, detections_for_image, existing_analysis, insert_analysis,
    stale_targets, AnalysisInput, StageSpec,
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
    // Per the detector ownership split: D-FINE → People/Vehicles, MegaDetector → Animals.
    vec![
        AnalysisInput {
            analyzer_id: "object_detection".into(),
            model_version: "dfine-m-v1".into(),
            payload: json!({"detections": [
                {"label": "car", "category": "Vehicles", "confidence": 0.77, "bbox": [5.0, 6.0, 7.0, 8.0]}
            ]}),
        },
        AnalysisInput {
            analyzer_id: "animal_detection".into(),
            model_version: "mdv5a-v1".into(),
            payload: json!({"detections": [
                {"label": "dog", "category": "Animals", "confidence": 0.91, "bbox": [1.0, 2.0, 3.0, 4.0]}
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

/// Seed a distinct present image (unique content-hash + path) and return its id.
fn seed_image_n(db: &Db, n: u8) -> i64 {
    let mut hash = vec![0u8; 32];
    hash[0] = n;
    db.conn
        .execute(
            "INSERT INTO images(content_hash, file_size, path, original_filename, imported_at)
             VALUES (?1, 1, ?2, ?3, 0)",
            params![hash, format!("p{n}.cr3"), format!("p{n}.cr3")],
        )
        .unwrap();
    db.conn.last_insert_rowid()
}

fn ok(analyzer_id: &str, model_version: &str) -> AnalysisInput {
    AnalysisInput {
        analyzer_id: analyzer_id.into(),
        model_version: model_version.into(),
        payload: json!({}),
    }
}

const OBJ: StageSpec = StageSpec {
    analyzer_id: "object_detection",
    model_version: "dfine-m-v1",
};
const CAP: StageSpec = StageSpec {
    analyzer_id: "caption",
    model_version: "florence-v1",
};

#[test]
fn stale_targets_per_stage_mask_and_status_gating() {
    let mut db = Db::open_in_memory().unwrap();
    let a = seed_image_n(&db, 1); // fully done
    let b = seed_image_n(&db, 2); // object done, caption stale
    let c = seed_image_n(&db, 3); // object errored, caption missing → both stale

    let tx = db.conn.transaction().unwrap();
    insert_analysis(
        &tx,
        a,
        0,
        &[
            ok("object_detection", "dfine-m-v1"),
            ok("caption", "florence-v1"),
        ],
    )
    .unwrap();
    insert_analysis(&tx, b, 0, &[ok("object_detection", "dfine-m-v1")]).unwrap();
    tx.commit().unwrap();
    // An errored object marker must NOT count as done (status='ok' gate) → stays stale.
    db.conn
        .execute(
            "INSERT OR REPLACE INTO analysis_results(image_id, analyzer_id, model_version, ran_at, status, payload)
             VALUES (?1, 'object_detection', 'dfine-m-v1', 0, 'error', '{}')",
            params![c],
        )
        .unwrap();

    let stages = [OBJ, CAP];
    let got = stale_targets(&db.conn, &stages, 0, 100).unwrap();
    // A is fully done → excluded; B and C returned in id order.
    let ids: Vec<i64> = got.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![b, c]);
    let bt = &got[0];
    assert_eq!(bt.stale, vec![false, true], "B: object done, caption stale");
    let ct = &got[1];
    assert_eq!(
        ct.stale,
        vec![true, true],
        "C: errored object + missing caption both stale"
    );
}

#[test]
fn stale_targets_keyset_pagination_and_version_bump() {
    let mut db = Db::open_in_memory().unwrap();
    let a = seed_image_n(&db, 1);
    let b = seed_image_n(&db, 2);
    let c = seed_image_n(&db, 3);
    // All three fully done at v1 for a single object stage.
    let tx = db.conn.transaction().unwrap();
    for id in [a, b, c] {
        insert_analysis(&tx, id, 0, &[ok("object_detection", "dfine-m-v1")]).unwrap();
    }
    tx.commit().unwrap();

    // Same version → nothing stale.
    assert!(stale_targets(&db.conn, &[OBJ], 0, 100).unwrap().is_empty());

    // Version bump → every image is stale for that stage.
    let v2 = StageSpec {
        analyzer_id: "object_detection",
        model_version: "dfine-m-v2",
    };
    // Keyset pagination: limit 2 then resume from the last id.
    let page1 = stale_targets(&db.conn, &[v2], 0, 2).unwrap();
    assert_eq!(page1.iter().map(|t| t.id).collect::<Vec<_>>(), vec![a, b]);
    let page2 = stale_targets(&db.conn, &[v2], page1.last().unwrap().id, 2).unwrap();
    assert_eq!(page2.iter().map(|t| t.id).collect::<Vec<_>>(), vec![c]);
    assert!(stale_targets(&db.conn, &[v2], c, 2).unwrap().is_empty());
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
    assert!(seen.contains(&(img, "animal_detection".into(), "mdv5a-v1".into())));
    assert!(seen.contains(&(img, "caption".into(), "florence-v1".into())));

    // Re-running the same analyzers is idempotent: no duplicate detection rows (car + dog), one
    // results row per analyzer.
    let tx = db.conn.transaction().unwrap();
    insert_analysis(&tx, img, 200, &records()).unwrap();
    tx.commit().unwrap();
    assert_eq!(detections_for_image(&db.conn, img).unwrap().len(), 2);
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM analysis_results", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
}

/// Migration 013's predicate (`json_extract(payload,'$.faces') = 0`) deletes ONLY zero-face
/// `face_detection` markers, regardless of payload formatting — verified against the real serialized
/// marker `insert_analysis` writes.
#[test]
fn json_extract_targets_only_zero_face_markers() {
    let mut db = Db::open_in_memory().unwrap();
    let a = seed_image_n(&db, 1);
    let b = seed_image_n(&db, 2);
    let tx = db.conn.transaction().unwrap();
    insert_analysis(
        &tx,
        a,
        0,
        &[AnalysisInput {
            analyzer_id: "face_detection".into(),
            model_version: "v1".into(),
            payload: json!({ "faces": 0 }),
        }],
    )
    .unwrap();
    insert_analysis(
        &tx,
        b,
        0,
        &[AnalysisInput {
            analyzer_id: "face_detection".into(),
            model_version: "v1".into(),
            payload: json!({ "faces": 2 }),
        }],
    )
    .unwrap();
    tx.commit().unwrap();

    let deleted = db
        .conn
        .execute(
            "DELETE FROM analysis_results WHERE analyzer_id='face_detection' \
             AND json_extract(payload,'$.faces')=0",
            [],
        )
        .unwrap();
    assert_eq!(deleted, 1, "only the zero-face marker is deleted");
    let remaining: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM analysis_results WHERE analyzer_id='face_detection'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 1, "the faces>0 marker is kept");
}
