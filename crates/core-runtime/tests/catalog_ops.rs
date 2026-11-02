//! Catalog reads and durable mutations through `CatalogPort` on an in-memory catalog. Delete
//! rejected uses an injected remover so the OS Trash is never touched.

use core_app::ports::{CatalogPort, EffectExecutor};
use core_app::workflow::{
    CullChange, CullFlag, Effect, Mutation, MutationReport, Operation, Outcome, RequestId,
    RequestToken,
};
use core_app::AppError;
use core_db::rusqlite::params;
use core_db::Db;
use core_model::catalog::QueryParams;
use core_model::identity::{CatalogEpoch, ImageId};
use core_runtime::catalog::{CatalogAdapter, SharedDb};
use core_runtime::workflow::InlineExecutor;
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    dir: PathBuf,
    db: SharedDb,
    adapter: CatalogAdapter,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "darkroom-catalog-ops-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db: SharedDb = Arc::new(Mutex::new(Db::open_in_memory().unwrap()));
        let adapter = CatalogAdapter::from_shared(db.clone())
            .with_trash(Arc::new(|p: &Path| std::fs::remove_file(p).is_ok()));
        Self { dir, db, adapter }
    }
    fn image(&self, tag: u8, name: &str) -> i64 {
        let path = self.dir.join(name);
        std::fs::write(&path, vec![tag; 16]).unwrap();
        let db = self.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                                    capture_date, imported_at)
                 VALUES (?1, 16, ?2, ?3, 'present', 1000, 0)",
                params![vec![tag; 32], path.display().to_string(), name],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }
    fn present(&self) -> Vec<i64> {
        let db = self.db.lock().unwrap();
        let mut stmt = db
            .conn
            .prepare("SELECT id FROM images WHERE status='present' ORDER BY id")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
        rows.map(Result::unwrap).collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn ids(values: &[i64]) -> Vec<ImageId> {
    values.iter().map(|v| ImageId(*v)).collect()
}

#[test]
fn keyword_add_remove_delete_round_trip() {
    let f = Fixture::new();
    let (a, b) = (f.image(1, "a.cr3"), f.image(2, "b.cr3"));
    f.adapter
        .mutate(&Mutation::AddKeyword {
            images: ids(&[a, b]),
            name: "  Sunset ".into(),
        })
        .unwrap();
    // Case-insensitive create-or-get: no duplicate keyword.
    f.adapter
        .mutate(&Mutation::AddKeyword {
            images: ids(&[a]),
            name: "sunset".into(),
        })
        .unwrap();
    let keywords = f.adapter.keywords().unwrap();
    assert_eq!(keywords.len(), 1);
    assert_eq!(
        (keywords[0].name.as_str(), keywords[0].count),
        ("Sunset", 2)
    );
    let tags = f.adapter.image_tags(a).unwrap();
    assert_eq!(tags.image, ImageId(a));
    assert_eq!(tags.keywords.len(), 1);

    let id = keywords[0].id;
    f.adapter
        .mutate(&Mutation::RemoveKeyword {
            images: ids(&[a]),
            keyword_id: id,
        })
        .unwrap();
    assert_eq!(f.adapter.keywords().unwrap()[0].count, 1);
    assert!(f.adapter.image_tags(a).unwrap().keywords.is_empty());

    f.adapter.mutate(&Mutation::DeleteKeyword(id)).unwrap();
    assert!(f.adapter.keywords().unwrap().is_empty());
    assert!(f.adapter.image_tags(b).unwrap().keywords.is_empty());
}

#[test]
fn static_and_smart_collections() {
    let f = Fixture::new();
    let (a, b, c) = (
        f.image(1, "a.cr3"),
        f.image(2, "b.cr3"),
        f.image(3, "c.cr3"),
    );
    let MutationReport::CollectionCreated(stat) = f
        .adapter
        .mutate(&Mutation::CreateCollection {
            name: " Best ".into(),
            smart: None,
            images: ids(&[a, b]),
        })
        .unwrap()
    else {
        panic!("expected CollectionCreated");
    };
    f.adapter
        .mutate(&Mutation::CreateCollection {
            name: "Picks".into(),
            smart: Some(Box::new(QueryParams {
                flag: Some("pick".into()),
                ..Default::default()
            })),
            images: vec![],
        })
        .unwrap();
    f.adapter
        .mutate(&Mutation::Cull {
            images: ids(&[a, c]),
            change: CullChange::Flag(CullFlag::Pick),
        })
        .unwrap();

    let rows = f.adapter.collections().unwrap();
    let best = rows.iter().find(|r| r.name == "Best").unwrap();
    let picks = rows.iter().find(|r| r.name == "Picks").unwrap();
    assert_eq!((best.is_smart, best.count), (false, 2));
    assert_eq!((picks.is_smart, picks.count), (true, 2));
    assert_eq!(picks.smart_query().unwrap().flag.as_deref(), Some("pick"));
    assert_eq!(f.adapter.image_tags(a).unwrap().collections.len(), 1);
    assert!(f.adapter.image_tags(c).unwrap().collections.is_empty());

    f.adapter
        .mutate(&Mutation::AddToCollection {
            id: stat,
            images: ids(&[c]),
        })
        .unwrap();
    f.adapter
        .mutate(&Mutation::RemoveFromCollection {
            id: stat,
            images: ids(&[a]),
        })
        .unwrap();
    f.adapter
        .mutate(&Mutation::RenameCollection {
            id: stat,
            name: "Keepers".into(),
        })
        .unwrap();
    let rows = f.adapter.collections().unwrap();
    let keepers = rows.iter().find(|r| r.id == stat).unwrap();
    assert_eq!((keepers.name.as_str(), keepers.count), ("Keepers", 2));

    f.adapter.mutate(&Mutation::DeleteCollection(stat)).unwrap();
    assert_eq!(f.adapter.collections().unwrap().len(), 1);
    // A blank name never creates a half-built collection.
    assert!(f
        .adapter
        .mutate(&Mutation::CreateCollection {
            name: "  ".into(),
            smart: None,
            images: ids(&[a]),
        })
        .is_err());
    assert_eq!(f.adapter.collections().unwrap().len(), 1);
}

#[test]
fn batch_cull_sets_rating_flag_and_label() {
    let f = Fixture::new();
    let (a, b, c) = (
        f.image(1, "a.cr3"),
        f.image(2, "b.cr3"),
        f.image(3, "c.cr3"),
    );
    f.adapter
        .mutate(&Mutation::Cull {
            images: ids(&[a, b]),
            change: CullChange::Rating(4),
        })
        .unwrap();
    f.adapter
        .mutate(&Mutation::Cull {
            images: ids(&[b]),
            change: CullChange::Flag(CullFlag::Reject),
        })
        .unwrap();
    f.adapter
        .mutate(&Mutation::Cull {
            images: ids(&[a]),
            change: CullChange::Label(Some(core_app::workflow::ColorLabel::Red)),
        })
        .unwrap();
    let row = |id| f.adapter.image(id).unwrap().unwrap();
    assert_eq!((row(a).stars, row(a).flag.as_str()), (4, "none"));
    assert_eq!(row(a).color_label.as_deref(), Some("red"));
    assert_eq!((row(b).stars, row(b).flag.as_str()), (4, "reject"));
    assert_eq!(row(c).stars, 0);
}

#[test]
fn pair_info_and_unlink() {
    let f = Fixture::new();
    let (raw, jpg) = (f.image(1, "a.cr3"), f.image(2, "a.jpg"));
    {
        let db = f.db.lock().unwrap();
        assert!(core_library::link_pair(&db.conn, raw, jpg).unwrap());
    }
    let primary = f.adapter.image_tags(raw).unwrap().pair.unwrap();
    assert_eq!(
        (primary.role.as_str(), primary.primary_id),
        ("primary", raw)
    );
    assert_eq!(primary.secondaries.len(), 1);
    let secondary = f.adapter.image_tags(jpg).unwrap().pair.unwrap();
    assert_eq!(secondary.role, "secondary");

    f.adapter
        .mutate(&Mutation::UnlinkPair {
            secondary: ImageId(jpg),
        })
        .unwrap();
    assert!(f.adapter.image_tags(raw).unwrap().pair.is_none());
    assert!(f.adapter.image_tags(jpg).unwrap().pair.is_none());
}

#[test]
fn delete_rejected_removes_only_rejected_rows_and_files() {
    let f = Fixture::new();
    let keep = f.image(1, "keep.cr3");
    let pick = f.image(2, "pick.cr3");
    let bad = f.image(3, "bad.cr3");
    let raw = f.image(4, "raw.cr3");
    let companion = f.image(5, "raw.jpg");
    {
        let db = f.db.lock().unwrap();
        core_library::set_flag(&db.conn, pick, "pick").unwrap();
        core_library::set_flag(&db.conn, bad, "reject").unwrap();
        core_library::set_flag(&db.conn, raw, "reject").unwrap();
        assert!(core_library::link_pair(&db.conn, raw, companion).unwrap());
    }
    let summary = f.adapter.reject_summary(&QueryParams::default()).unwrap();
    assert_eq!(
        (summary.images, summary.companions, summary.bytes),
        (2, 1, 48)
    );

    let MutationReport::RejectedDeleted(result) = f
        .adapter
        .mutate(&Mutation::DeleteRejected(Box::default()))
        .unwrap()
    else {
        panic!("expected RejectedDeleted");
    };
    assert_eq!(
        (result.trashed, result.failed, result.companions),
        (3, 0, 1)
    );
    assert_eq!(result.hashes.len(), 3);
    assert_eq!(f.present(), vec![keep, pick]);
    for gone in ["bad.cr3", "raw.cr3", "raw.jpg"] {
        assert!(!f.dir.join(gone).exists(), "{gone}");
    }
    for kept in ["keep.cr3", "pick.cr3"] {
        assert!(f.dir.join(kept).exists(), "{kept}");
    }
    assert_eq!(
        f.adapter
            .reject_summary(&QueryParams::default())
            .unwrap()
            .images,
        0
    );
}

#[test]
fn delete_rejected_on_replaced_catalog_leaves_new_catalog_untouched() {
    let old = Fixture::new();
    let bad = old.image(1, "bad.cr3");
    {
        let db = old.db.lock().unwrap();
        core_library::set_flag(&db.conn, bad, "reject").unwrap();
    }
    // A second catalog holding the identical rejected row (same id, path, hash): a finalize that
    // leaked into it would delete this row.
    let replacement = old.dir.join("replacement.db");
    {
        let db = Db::open(&replacement).unwrap();
        let path = old.dir.join("bad.cr3");
        db.conn
            .execute(
                "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                                    capture_date, imported_at)
                 VALUES (?1, 16, ?2, 'bad.cr3', 'present', 1000, 0)",
                params![vec![1u8; 32], path.display().to_string()],
            )
            .unwrap();
        core_library::set_flag(&db.conn, db.conn.last_insert_rowid(), "reject").unwrap();
    }
    // The remover runs in the unlocked file phase: swap the catalog there.
    let handle: Arc<OnceLock<Weak<CatalogAdapter>>> = Arc::new(OnceLock::new());
    let swap = handle.clone();
    let target = replacement.clone();
    let adapter = Arc::new(
        CatalogAdapter::from_shared(old.db.clone()).with_trash(Arc::new(move |p: &Path| {
            let ok = std::fs::remove_file(p).is_ok();
            let adapter = swap.get().unwrap().upgrade().unwrap();
            adapter.close().unwrap();
            adapter.open(&target).unwrap();
            ok
        })),
    );
    handle.set(Arc::downgrade(&adapter)).unwrap();

    assert_eq!(
        adapter.mutate(&Mutation::DeleteRejected(Box::default())),
        Err(AppError::Stale)
    );
    let rows = adapter.query(&QueryParams::default()).unwrap();
    assert_eq!(rows.len(), 1, "the new catalog's row survives");
}

#[test]
fn inline_executor_maps_new_operations() {
    let f = Fixture::new();
    let a = f.image(1, "a.cr3");
    let run = |operation| {
        InlineExecutor
            .execute(
                Effect {
                    token: RequestToken {
                        catalog_epoch: CatalogEpoch::default(),
                        request_id: RequestId(1),
                    },
                    operation,
                },
                &f.adapter,
            )
            .result
    };
    assert!(matches!(
        run(Operation::Mutate(Mutation::AddKeyword {
            images: ids(&[a]),
            name: "k".into()
        })),
        Ok(Outcome::Mutated(MutationReport::Done))
    ));
    assert!(matches!(run(Operation::LoadKeywords), Ok(Outcome::Keywords(k)) if k.len() == 1));
    assert!(matches!(run(Operation::LoadCollections), Ok(Outcome::Collections(c)) if c.is_empty()));
    assert!(matches!(
        run(Operation::LoadImageTags(ImageId(a))),
        Ok(Outcome::ImageTags(t)) if t.image == ImageId(a) && t.keywords.len() == 1
    ));
    assert!(matches!(
        run(Operation::LoadRejectSummary(Box::default())),
        Ok(Outcome::RejectSummary(s)) if s.images == 0
    ));
}

#[test]
fn mutations_on_a_closed_catalog_report_not_open() {
    let f = Fixture::new();
    f.adapter.close().unwrap();
    assert_eq!(
        f.adapter.mutate(&Mutation::DeleteKeyword(1)),
        Err(AppError::NotOpen)
    );
    assert_eq!(
        f.adapter.mutate(&Mutation::DeleteRejected(Box::default())),
        Err(AppError::NotOpen)
    );
}

#[test]
fn remove_from_library_drops_rows_but_keeps_files() {
    let f = Fixture::new();
    let (keep, gone) = (f.image(1, "keep.cr3"), f.image(2, "gone.cr3"));
    let companion = f.image(3, "gone.jpg");
    {
        let db = f.db.lock().unwrap();
        assert!(core_library::link_pair(&db.conn, gone, companion).unwrap());
    }
    let MutationReport::RemovedFromLibrary(result) = f
        .adapter
        .mutate(&Mutation::RemoveFromLibrary {
            images: ids(&[gone]),
        })
        .unwrap()
    else {
        panic!("expected RemovedFromLibrary");
    };
    assert_eq!((result.removed, result.companions), (2, 1));
    assert_eq!(f.present(), vec![keep]);
    for name in ["keep.cr3", "gone.cr3", "gone.jpg"] {
        assert!(f.dir.join(name).exists(), "{name}");
    }
}

#[test]
fn save_receipt_edited_at_is_the_stored_version_and_strictly_increases() {
    let f = Fixture::new();
    let id = f.image(7, "a.cr3");
    let hash = "07".repeat(32);
    let stored = |f: &Fixture| {
        let db = f.db.lock().unwrap();
        core_library::edits::get_edit_with_version(&db.conn, id)
            .unwrap()
            .map(|(_, updated_at)| updated_at)
    };
    let first = f.adapter.save_edit(id, &hash, 1, "{}").unwrap();
    assert_eq!(Some(first.edited_at), stored(&f));
    let second = f.adapter.save_edit(id, &hash, 1, "{\"a\":1}").unwrap();
    assert_eq!(Some(second.edited_at), stored(&f));
    assert!(second.edited_at > first.edited_at);
}

#[test]
fn presets_snapshots_and_settings_resolve_through_the_adapter() {
    use core_app::workflow::SettingsSource;
    use core_model::DevelopParams;

    let f = Fixture::new();
    let image = ImageId(f.image(1, "a.cr3"));
    {
        let db = f.db.lock().unwrap();
        core_library::insert_preset(
            &db.conn,
            "Bright",
            "My Presets",
            false,
            r#"["exposure"]"#,
            r#"{"exposure": 2.0}"#,
            6,
            0,
        )
        .unwrap();
    }
    let listed = f.adapter.presets().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "Bright");

    let base = DevelopParams {
        exposure: 1.0,
        contrast: 30.0,
        ..Default::default()
    };
    let half = f
        .adapter
        .resolve_settings(&SettingsSource::Preset(listed[0].id), &base, 0.5, false)
        .unwrap();
    assert!((half.exposure - 1.5).abs() < 1e-5, "got {}", half.exposure);
    assert_eq!(half.contrast, 30.0);
    assert_eq!(
        f.adapter
            .resolve_settings(&SettingsSource::Preset(999), &base, 1.0, false),
        Err(AppError::InvalidCommand("preset not found".into()))
    );

    let create = |name: &str| {
        f.adapter
            .mutate(&Mutation::CreateSnapshot {
                image,
                name: name.into(),
                params: Arc::new(base.clone()),
            })
            .unwrap()
    };
    let MutationReport::SnapshotCreated(id) = create("Look") else {
        panic!("expected SnapshotCreated");
    };
    let MutationReport::SnapshotCreated(second) = create("Look") else {
        panic!("expected SnapshotCreated");
    };
    let names: Vec<_> = f
        .adapter
        .snapshots(image.0)
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(
        names,
        ["Look (2)", "Look"],
        "unique per image, newest first"
    );

    // A snapshot restores whole, ignoring amount and replace_all.
    let restored = f
        .adapter
        .resolve_settings(
            &SettingsSource::Snapshot(id),
            &DevelopParams::default(),
            0.1,
            true,
        )
        .unwrap();
    assert_eq!(restored, base);

    f.adapter
        .mutate(&Mutation::RenameSnapshot {
            id,
            name: "  Renamed ".into(),
        })
        .unwrap();
    f.adapter.mutate(&Mutation::DeleteSnapshot(second)).unwrap();
    let left = f.adapter.snapshots(image.0).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].name, "Renamed");
}
