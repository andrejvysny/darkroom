#[path = "support/migration_catalog.rs"]
mod migration_catalog;

use core_db::Db;
use core_library::{query_images, QueryParams};
use migration_catalog::{CORRUPT_EDIT, PV5, PV6};
use std::{fs, path::PathBuf};

fn fixture() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "darkroom-migration-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    migration_catalog::generate(&root).unwrap();
    root
}

#[test]
fn copied_catalog_preserves_queries_and_relationships_after_reopen() {
    let root = fixture();
    for name in ["source.db", "copied.db"] {
        let db = Db::open(&root.join(name)).unwrap();
        assert_eq!(
            db.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            25
        );
        assert_edits(&db.conn);
        assert_queries(&db.conn);
        assert_sources(&db.conn);
        assert_integrity(&db.conn);
    }
    assert!(!root.join("photos/missing.cr3").exists());
    let raw = root.join("photos/raw.cr3");
    let sidecar = core_library::sidecar::read_sidecar(raw.to_str().unwrap()).unwrap();
    assert_eq!(sidecar.schema_version, 1);
    assert_eq!(sidecar.process_version, Some(5));
    assert_eq!(
        sidecar.develop.unwrap(),
        serde_json::from_str::<serde_json::Value>(PV5).unwrap()
    );
    assert_eq!(image::open(root.join("photos/raw.jpg")).unwrap().width(), 2);
    fs::remove_dir_all(root).unwrap();
}

fn assert_edits(conn: &core_db::rusqlite::Connection) {
    assert_eq!(
        core_library::get_edit(conn, 1).unwrap().as_deref(),
        Some(PV5)
    );
    assert_eq!(
        core_library::get_edit(conn, 2).unwrap().as_deref(),
        Some(PV6)
    );
    assert_eq!(
        core_library::get_edit(conn, 6).unwrap().as_deref(),
        Some(CORRUPT_EDIT)
    );
    assert_eq!(
        core_library::get_snapshot_params(conn, 1).unwrap(),
        Some((PV5.to_owned(), 5))
    );
    for json in [PV5, PV6] {
        let edit: core_model::DevelopParams = serde_json::from_str(json).unwrap();
        assert_eq!(edit.masks.len(), 1);
        assert_eq!(edit.masks[0].components.len(), 1);
        assert_eq!(edit.masks[0].adjust.exposure, 0.8);
        assert!(edit.denoise.enabled);
        assert!(edit.denoise.amount > 0.0);
    }
    let edit: core_model::DevelopParams = serde_json::from_str(PV5).unwrap();
    assert_eq!(edit.crop.angle, 2.0);
    assert_eq!(edit.crop.hw, 0.4);
    let edit: core_model::DevelopParams = serde_json::from_str(PV6).unwrap();
    assert_eq!(edit.crop.rot90, 1);
    let preset = core_library::get_preset(conn, 1).unwrap().unwrap();
    assert_eq!(preset.params, "{\"exposure\":0.5}");
    assert_eq!(preset.process_version, 5);
    for (id, version) in [(1, 5), (2, 6)] {
        let stored: i64 = conn
            .query_row(
                "SELECT process_version FROM edits WHERE image_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored, version);
    }
}

fn assert_queries(conn: &core_db::rusqlite::Connection) {
    let rows = query_images(
        conn,
        &QueryParams {
            include_paired: Some(true),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(rows
        .iter()
        .any(|r| r.id == 1 && r.camera_model.as_deref() == Some("Camera") && r.paired_count == 1));
    assert!(rows.iter().any(|r| r.id == 2 && r.paired_to == Some(1)));
    for filter in [
        QueryParams {
            keyword_id: Some(1),
            ..Default::default()
        },
        QueryParams {
            collection_id: Some(1),
            ..Default::default()
        },
        QueryParams {
            person_id: Some(1),
            ..Default::default()
        },
        QueryParams {
            import_session_id: Some(1),
            ..Default::default()
        },
    ] {
        assert!(query_images(conn, &filter)
            .unwrap()
            .iter()
            .any(|r| r.id == 1));
    }
}

fn assert_integrity(conn: &core_db::rusqlite::Connection) {
    for table in ["hdr_sources", "panorama_sources"] {
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2);
    }
    let bad: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(bad, 0);
}

fn assert_sources(conn: &core_db::rusqlite::Connection) {
    for (id, kind) in [(3, "hdr"), (4, "panorama")] {
        let merged = core_library::merge_sources(conn, id).unwrap().unwrap();
        assert_eq!(merged.kind, kind);
        assert_eq!(merged.sources.len(), 2);
        assert_eq!(
            (
                merged.sources[0].image_id,
                merged.sources[0].position,
                merged.sources[0].present
            ),
            (1, 0, true)
        );
        assert_eq!(
            (
                merged.sources[1].image_id,
                merged.sources[1].position,
                merged.sources[1].present
            ),
            (5, 1, false)
        );
        if id == 3 {
            assert_eq!(merged.sources[0].relative_ev, Some(-1.0));
            assert_eq!(merged.sources[1].relative_ev, Some(1.0));
        }
    }
}

#[test]
fn refusal_preserves_invalid_inputs_and_existing_output() {
    let root = fixture();
    for name in ["corrupt.db", "newer.db"] {
        let path = root.join(name);
        let before = fs::read(&path).unwrap();
        let error = match Db::open(&path) {
            Ok(_) => panic!("invalid catalog accepted"),
            Err(error) => error,
        };
        if name == "newer.db" {
            assert!(matches!(
                error,
                core_db::DbError::SchemaTooNew {
                    found: 26,
                    supported: 25
                }
            ));
        } else {
            assert!(matches!(
                error,
                core_db::DbError::Sqlite(_) | core_db::DbError::Corrupt(_)
            ));
        }
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let corrupt = fs::read(root.join("photos/corrupt.cr3")).unwrap();
    assert!(migration_catalog::generate(&root).is_err());
    assert_eq!(fs::read(root.join("photos/corrupt.cr3")).unwrap(), corrupt);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn sidecar_edit_payloads_roundtrip_with_model_and_preserve_future_fields() {
    use core_model::document::{EditDocument, ReadStatus};
    let root = fixture();
    let raw = root.join("photos/raw.cr3");
    let raw = raw.to_str().unwrap();
    let mut sidecar = core_library::sidecar::read_sidecar(raw).unwrap();
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../../tests/migration/model/inputs.json")).unwrap();
    for fixture in fixtures.as_array().unwrap() {
        sidecar.develop = Some(fixture["params"].clone());
        core_library::sidecar::write_snapshot(&core_library::sidecar::SidecarWrite {
            path: core_library::sidecar::sidecar_path(raw),
            sidecar: sidecar.clone(),
        })
        .unwrap();
        let restored = core_library::sidecar::read_sidecar(raw).unwrap();
        assert_eq!(restored, sidecar);
        let json = serde_json::to_string(restored.develop.as_ref().unwrap()).unwrap();
        assert_eq!(EditDocument::read(json, 5).status(), &ReadStatus::Supported);
    }
    sidecar.develop = Some(serde_json::json!({"future":{"amount":0.5}}));
    sidecar.process_version = Some(7);
    core_library::sidecar::write_snapshot(&core_library::sidecar::SidecarWrite {
        path: core_library::sidecar::sidecar_path(raw),
        sidecar: sidecar.clone(),
    })
    .unwrap();
    let restored = core_library::sidecar::read_sidecar(raw).unwrap();
    assert_eq!(restored, sidecar);
    let json = serde_json::to_string(&restored.develop.unwrap()).unwrap();
    let document = EditDocument::read(&json, restored.process_version.unwrap());
    assert_eq!(document.status(), &ReadStatus::UnsupportedVersion);
    assert_eq!(document.raw_json(), json);
    assert!(document.preview().is_none());
    fs::remove_dir_all(root).unwrap();
}
