//! Trash-flow protocol tests: plan retires identities, unlocked IO, fenced publish.
use super::*;
use core_library::QueryParams;
use core_runtime::publication::{PublicationGate, SourceKey};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("darkroom-trash-flow-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fix {
    dir: PathBuf,
    gate: PublicationGate,
    db: Mutex<Db>,
}

impl Fix {
    fn new(name: &str) -> Self {
        let dir = tempdir(name);
        let db = Mutex::new(Db::open(&dir.join("catalog.db")).unwrap());
        Self {
            dir,
            gate: PublicationGate::default(),
            db,
        }
    }

    /// Real file + present row (tag = content-hash byte); returns the image id.
    fn insert_file(&self, tag: u8, filename: &str) -> i64 {
        let path = self.dir.join(filename);
        std::fs::write(&path, vec![tag; 64]).unwrap();
        let db = self.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO images(content_hash, file_size, path, original_filename,
                                    status, capture_date, imported_at)
                 VALUES (?1, 64, ?2, ?3, 'present', 1000, 0)",
                core_db::rusqlite::params![vec![tag; 32], path.display().to_string(), filename],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    fn set_flag(&self, id: i64, flag: &str) {
        let db = self.db.lock().unwrap();
        core_library::set_flag(&db.conn, id, flag).unwrap();
    }

    fn row_count(&self, id: i64) -> i64 {
        let db = self.db.lock().unwrap();
        db.conn
            .query_row("SELECT count(*) FROM images WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap()
    }
}

/// Injected remover that really deletes, so the Trash is never touched.
fn fs_remover() -> impl Fn(&Path) -> bool {
    |p: &Path| std::fs::remove_file(p).is_ok()
}

fn key_for(tag: u8, path: &str) -> SourceKey {
    SourceKey {
        hash: (0..32).map(|_| format!("{tag:02x}")).collect(),
        path: path.to_string(),
    }
}

/// Retiring at plan time must fail the fence on anything still publishing from the old
/// identity — that is what makes the unlocked IO safe.
#[test]
fn retired_target_rejects_stale_publication() {
    let f = Fix::new("retire");
    let id = f.insert_file(1, "a.cr3");
    f.set_flag(id, "reject");
    let token = f
        .gate
        .capture(
            ImageId(id),
            key_for(1, &f.dir.join("a.cr3").display().to_string()),
        )
        .expect("fresh identity captures");

    plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();

    let err = f.gate.with_current(&token, || ());
    assert_eq!(err.err(), Some(core_runtime::publication::Stale));
}

/// A reset while the file IO runs must leave the new catalog's rows untouched (Stale path).
#[test]
fn catalog_reset_during_trash_leaves_new_catalog_untouched() {
    let f = Fix::new("reset");
    let id = f.insert_file(2, "b.cr3");
    f.set_flag(id, "reject");
    let run = plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();
    let (gone, failed) = trash_reject(&run.plan, &fs_remover());
    assert_eq!(gone.len(), 1, "the file went to the Trash");

    // Simulate `database_reset`'s retirement landing mid-flight.
    drop(f.gate.invalidate_all_guard());

    let published = publish_reject(&f.gate, &f.db, &run, &gone, failed, &|_, _| {}).unwrap();
    assert!(matches!(published, Publish::Stale));
    assert_eq!(
        f.row_count(id),
        1,
        "finalize must not touch the new catalog"
    );
}

/// The file IO must run with BOTH the catalog lock and the publication gate released.
#[test]
fn trash_runs_without_db_lock() {
    let f = Fix::new("unlocked");
    let id = f.insert_file(3, "c.cr3");
    f.set_flag(id, "reject");
    let run = plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();

    let ran = AtomicBool::new(false);
    let (gone, failed) = trash_reject(&run.plan, &|p: &Path| {
        assert!(f.db.try_lock().is_ok(), "catalog lock held during file IO");
        let gate = f.gate.commit_guard(f.gate.epoch());
        assert!(gate.is_ok(), "publication gate held during file IO");
        drop(gate);
        ran.store(true, Ordering::SeqCst);
        std::fs::remove_file(p).is_ok()
    });

    assert!(ran.load(Ordering::SeqCst), "remover must have been called");
    assert_eq!((gone.len(), failed), (1, 0));
    assert!(!f.dir.join("c.cr3").exists());
}

/// Happy publish: gone rows are removed, only the planned targets' sources are updated (an
/// untouched row's token stays publishable — publish no longer retires the whole catalog),
/// and only hashes with no remaining present row become orphan thumbnails.
#[test]
fn publish_finalizes_and_filters_orphan_thumbs() {
    let f = Fix::new("publish");
    let a = f.insert_file(4, "a.cr3");
    let b = f.insert_file(5, "b.cr3");
    let shared_keeper = f.insert_file(5, "b-keeper.cr3"); // same hash as b
    f.set_flag(a, "reject");
    f.set_flag(b, "reject");
    let keeper_token = f
        .gate
        .capture(
            ImageId(shared_keeper),
            key_for(5, &f.dir.join("b-keeper.cr3").display().to_string()),
        )
        .expect("fresh identity captures");

    let run = plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();
    let (gone, failed) = trash_reject(&run.plan, &fs_remover());
    let published = publish_reject(&f.gate, &f.db, &run, &gone, failed, &|_, _| {}).unwrap();

    let (res, orphans) = match published {
        Publish::Published(res, orphans) => (res, orphans),
        Publish::Stale => panic!("epoch was never invalidated mid-flight"),
    };
    assert_eq!(
        (res.trashed, res.failed, res.changed, res.companions),
        (2, 0, 0, 0)
    );
    assert_eq!(f.row_count(a), 0);
    assert_eq!(f.row_count(b), 0);
    assert_eq!(f.row_count(shared_keeper), 1);
    assert_eq!(
        orphans,
        vec![(0..32).map(|_| "04".to_string()).collect::<String>()],
        "only the hash with no present row left is orphaned"
    );
    assert_eq!(
        f.gate.with_current(&keeper_token, || ()),
        Ok(()),
        "publish only touches the planned targets' sources; an untouched row keeps its \
         identity — the epoch bump that `invalidate_all` used to fire is reserved for \
         true catalog resets and would abort concurrent imports"
    );
}

/// A cull/dedup delete mutates the catalog, it does not replace it: the publish phase must
/// leave the catalog epoch alone, or every concurrent import/index fenced on
/// `commit_guard(epoch)` would abort as stale after an ordinary delete.
#[test]
fn publish_does_not_change_catalog_epoch() {
    let f = Fix::new("epoch");
    let id = f.insert_file(6, "e.cr3");
    f.set_flag(id, "reject");

    let before = f.gate.epoch();
    let run = plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();
    let (gone, failed) = trash_reject(&run.plan, &fs_remover());
    let published = publish_reject(&f.gate, &f.db, &run, &gone, failed, &|_, _| {}).unwrap();
    assert!(matches!(published, Publish::Published(_, _)));

    assert_eq!(
        f.gate.epoch(),
        before,
        "epoch before plan must equal epoch after publish — only `database_reset` bumps it"
    );
}

/// A target whose file failed to trash keeps its row — so its publication identity, which
/// the plan retired, must be re-registered with the row's current path + hash at publish,
/// or the image stays unpublishable forever despite being fully present.
#[test]
fn kept_target_is_publishable_again() {
    let f = Fix::new("kept-publishable");
    let kept = f.insert_file(7, "stays.cr3");
    let removed = f.insert_file(8, "leaves.cr3");
    f.set_flag(kept, "reject");
    f.set_flag(removed, "reject");

    // The remover fails for `kept` (its file stays) and succeeds for `removed`.
    let stays = f.dir.join("stays.cr3");
    let run = plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();
    let (gone, failed) = trash_reject(&run.plan, &|p: &Path| {
        if p == stays.as_path() {
            false
        } else {
            std::fs::remove_file(p).is_ok()
        }
    });
    let published = publish_reject(&f.gate, &f.db, &run, &gone, failed, &|_, _| {}).unwrap();
    assert!(matches!(published, Publish::Published(_, _)));
    assert_eq!(failed, 1, "the kept target failed to trash");
    assert_eq!(f.row_count(kept), 1, "kept row survives finalize");

    let key = key_for(7, &f.dir.join("stays.cr3").display().to_string());
    let token = f
        .gate
        .capture(ImageId(kept), key)
        .expect("kept target is publishable again after publish");
    assert_eq!(f.gate.with_current(&token, || ()), Ok(()));
}

/// An id whose row was deleted at publish must stay retired: a token captured before the plan
/// fails the fence afterwards, and the identity cannot be captured again.
#[test]
fn removed_target_token_is_stale() {
    let f = Fix::new("removed-stale");
    let id = f.insert_file(9, "gone.cr3");
    f.set_flag(id, "reject");
    let token = f
        .gate
        .capture(
            ImageId(id),
            key_for(9, &f.dir.join("gone.cr3").display().to_string()),
        )
        .expect("fresh identity captures");

    let run = plan_reject(&f.gate, &f.db, &QueryParams::default()).unwrap();
    let (gone, failed) = trash_reject(&run.plan, &fs_remover());
    let published = publish_reject(&f.gate, &f.db, &run, &gone, failed, &|_, _| {}).unwrap();
    assert!(matches!(published, Publish::Published(_, _)));
    assert_eq!(f.row_count(id), 0);

    assert_eq!(
        f.gate.with_current(&token, || ()).err(),
        Some(core_runtime::publication::Stale),
        "a removed row's token is dead after publish"
    );
    assert_eq!(
        f.gate
            .capture(
                ImageId(id),
                key_for(9, &f.dir.join("gone.cr3").display().to_string()),
            )
            .err(),
        Some(core_runtime::publication::Stale),
        "a removed row cannot be published from again"
    );
}
