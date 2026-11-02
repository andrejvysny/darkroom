//! Real-catalog tests for [`write_one`]. [`Fix`] is also re-exported through
//! [`crate::sidecar::test_support`] so the hosts' `sidecar_queue` tests can reuse the catalog instead of
//! duplicating it.
use super::*;
use core_db::rusqlite::params;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Fix {
    dir: PathBuf,
    pub gate: PublicationGate,
    pub db: std::sync::Arc<Mutex<Db>>,
}

impl Fix {
    pub fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("darkroom-sidecar-write-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let catalog = Db::open(&dir.join("catalog.db")).unwrap();
        Self {
            dir,
            gate: PublicationGate::default(),
            db: std::sync::Arc::new(Mutex::new(catalog)),
        }
    }

    /// Re-open the same catalog file as a fresh handle: the app restart case, where nothing
    /// in-memory (queue, gate, worker) survives but the catalog and the disk do.
    pub(crate) fn reopened(&self) -> Self {
        Self {
            dir: self.dir.clone(),
            gate: PublicationGate::default(),
            db: std::sync::Arc::new(Mutex::new(Db::open(&self.dir.join("catalog.db")).unwrap())),
        }
    }

    /// Real file + `present` row on disk; `tag` is the content-hash byte. Returns the image id.
    pub fn insert_image(&self, tag: u8, filename: &str) -> i64 {
        let path = self.dir.join(filename);
        std::fs::write(&path, vec![tag; 64]).unwrap();
        let db = self.db.lock().unwrap();
        db.conn
            .execute(
                "INSERT INTO images(content_hash, file_size, path, original_filename,
                                    status, capture_date, imported_at)
                 VALUES (?1, 64, ?2, ?3, 'present', 1000, 0)",
                params![vec![tag; 32], path.display().to_string(), filename],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    pub(crate) fn dir(&self) -> &PathBuf {
        &self.dir
    }

    fn path(&self, filename: &str) -> String {
        self.dir.join(filename).display().to_string()
    }

    fn key(&self, tag: u8, filename: &str) -> SourceKey {
        SourceKey {
            hash: (0..32).map(|_| format!("{tag:02x}")).collect(),
            path: self.path(filename),
        }
    }

    fn delete(&self, id: i64) {
        self.db
            .lock()
            .unwrap()
            .conn
            .execute("DELETE FROM images WHERE id = ?1", [id])
            .unwrap();
    }

    fn set_status(&self, id: i64, status: &str) {
        self.db
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE images SET status = ?2 WHERE id = ?1",
                params![id, status],
            )
            .unwrap();
    }

    /// The file on disk changed: same row, different bytes.
    fn repoint(&self, id: i64, tag: u8) {
        self.db
            .lock()
            .unwrap()
            .conn
            .execute(
                "UPDATE images SET content_hash = ?2 WHERE id = ?1",
                params![id, vec![tag; 32]],
            )
            .unwrap();
    }

    /// An edit landed (rating), leaving source identity untouched.
    fn set_stars(&self, id: i64, stars: i64) {
        let db = self.db.lock().unwrap();
        core_library::set_rating(&db.conn, id, stars).unwrap();
    }

    pub fn sidecar(&self, filename: &str) -> Option<core_library::Sidecar> {
        core_library::sidecar::read_sidecar(&self.path(filename))
    }
}

/// Nothing was published (no sidecar) and no staged temp was left behind.
fn assert_no_publication(f: &Fix) {
    let leftovers: Vec<String> = std::fs::read_dir(f.dir())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp") || n.ends_with(".json"))
        .collect();
    assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
}

/// A `write` that records it was reached at all, without touching the filesystem.
fn recording(called: &AtomicBool) -> impl Fn(&SidecarWrite) -> std::io::Result<()> + '_ {
    move |_| {
        called.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// The real write, plus a record of whether it was reached at all.
fn recorded(called: &AtomicBool) -> impl Fn(&SidecarWrite) -> std::io::Result<()> + '_ {
    move |snapshot| {
        called.store(true, Ordering::SeqCst);
        production_write(snapshot)
    }
}

#[test]
fn unrelated_insert_does_not_drop_pending_sidecar() {
    let f = Fix::new("unrelated-insert");
    let a = f.insert_image(1, "a.cr3");
    let b = f.insert_image(2, "b.cr3");
    f.set_stars(a, 4);
    // The watcher registered two other images: the catalog-wide sequence moved twice while A's
    // sidecar was queued. The old pre-read `capture_at` turned that into "sidecar skipped".
    f.gate.replace_source(ImageId(b), f.key(2, "b.cr3"));
    f.gate
        .replace_source(ImageId(999_999), f.key(7, "unindexed.cr3"));
    let called = AtomicBool::new(false);
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| {}).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Written));
    assert!(called.load(Ordering::SeqCst));
    let sc = f.sidecar("a.cr3").expect("A's sidecar mirrors its rating");
    assert_eq!(sc.rating.stars, 4);

    // And a full catalog-wide invalidation (the watcher re-registering everything) must not
    // strand A's next edit either: the mark still reaches disk.
    drop(f.gate.invalidate_all_guard());
    f.gate.replace_source(ImageId(b), f.key(2, "b.cr3"));
    f.set_stars(a, 5);
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| {}).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Written), "{outcome:?}");
    assert_eq!(f.sidecar("a.cr3").expect("re-written").rating.stars, 5);
}

/// The exact shape of the old defect: a catalog-wide mutation lands while A is between capture
/// and publish. Old code logged "sidecar skipped: source identity stale" and counted the id as
/// done, so A's edit never reached disk. Now the id comes back as `Retry` and the next pass
/// writes it.
#[test]
fn epoch_move_mid_publish_keeps_the_intent() {
    let f = Fix::new("epoch-mid-publish");
    let a = f.insert_image(20, "a.cr3");
    f.set_stars(a, 3);
    let called = AtomicBool::new(false);
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| {
        drop(f.gate.invalidate_all_guard());
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Retry), "{outcome:?}");
    assert_no_publication(&f);
    // The queue would re-mark and retry; the edit does get mirrored.
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| {}).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Written), "{outcome:?}");
    assert_eq!(
        f.sidecar("a.cr3")
            .expect("sidecar after retry")
            .rating
            .stars,
        3
    );
}

#[test]
fn deleted_source_is_gone_and_writes_nothing() {
    let f = Fix::new("deleted");
    let deleted = f.insert_image(3, "deleted.cr3");
    let missing = f.insert_image(4, "missing.cr3");
    let called = AtomicBool::new(false);
    // Row removed between the snapshot and the fenced re-read.
    let outcome = write_one(&f.gate, &f.db, deleted, &recording(&called), &|| {
        f.delete(deleted)
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Gone));
    // Row still there but no longer a file to mirror.
    let outcome = write_one(&f.gate, &f.db, missing, &recording(&called), &|| {
        f.set_status(missing, "missing")
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Unavailable));
    // Never there at all.
    let outcome = write_one(&f.gate, &f.db, 99_999, &recording(&called), &|| {}).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Gone));
    assert_no_publication(&f);
    assert!(f.sidecar("deleted.cr3").is_none());
    assert!(f.sidecar("missing.cr3").is_none());
}

#[test]
fn replaced_bytes_while_paused_is_gone() {
    let f = Fix::new("replaced-bytes");
    let a = f.insert_image(5, "a.cr3");
    let called = AtomicBool::new(false);
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| f.repoint(a, 9)).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Gone));
    assert_no_publication(&f);
    assert!(f.sidecar("a.cr3").is_none());
}

#[test]
fn write_failure_is_failed_not_written() {
    let f = Fix::new("write-failure");
    let a = f.insert_image(6, "a.cr3");
    let outcome = write_one(
        &f.gate,
        &f.db,
        a,
        &|_| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        &|| {},
    )
    .unwrap();
    assert!(
        matches!(&outcome, SidecarOutcome::Failed(text) if text.contains("denied")),
        "{outcome:?}"
    );
    assert!(f.sidecar("a.cr3").is_none());
    // The production writer keeps the OS error kind (it becomes the `Failed` text the queue
    // logs), so a read-only volume stays diagnosable instead of a generic failure.
    let orphan = core_library::sidecar_snapshot(&f.db.lock().unwrap().conn, a)
        .unwrap()
        .map(|mut w| {
            w.path = f.dir.join("no-such-dir").join("a.cr3.json");
            w
        })
        .expect("snapshot");
    let error = production_write(&orphan).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn edit_during_publish_is_retry() {
    let f = Fix::new("edit-during-publish");
    let a = f.insert_image(8, "a.cr3");
    f.set_stars(a, 2);
    let called = AtomicBool::new(false);
    // Same bytes, newer edit: publishing the sidecar we gathered would roll the rating back.
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| f.set_stars(a, 5)).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Retry));
    assert_no_publication(&f);
    assert_eq!(f.sidecar("a.cr3").map(|sc| sc.rating.stars), None);
}

#[test]
fn fence_moved_over_live_row_is_retry() {
    let f = Fix::new("fence-live");
    let a = f.insert_image(10, "a.cr3");
    let called = AtomicBool::new(false);
    // Identity mutated in the gate (source replacement) while we were between capture and publish.
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| {
        f.gate.replace_source(ImageId(a), f.key(11, "a.cr3"));
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Retry));
    assert_no_publication(&f);
}

#[test]
fn fence_moved_over_deleted_row_is_gone() {
    let f = Fix::new("fence-deleted");
    let a = f.insert_image(12, "a.cr3");
    let called = AtomicBool::new(false);
    let outcome = write_one(&f.gate, &f.db, a, &recorded(&called), &|| {
        f.gate.replace_source(ImageId(a), f.key(13, "a.cr3"));
        f.delete(a);
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Gone));
    assert_no_publication(&f);
}

#[test]
fn failed_write_log_line_never_carries_a_path() {
    let redacted = safe_write_error(
        "Failed to write \"/Volumes/Photos/DCIM/a.cr3.json.tmp\": Read-only file system (os error 30)",
    );
    assert!(!redacted.contains("Volumes"));
    assert!(redacted.contains("Read-only file system (os error 30)"));
    assert_eq!(
        safe_write_error("Permission denied"),
        "Permission denied".to_string()
    );
}

#[test]
fn staged_temp_is_unique_hidden_and_renamed_away_on_success() {
    let f = Fix::new("staging-success");
    let a = f.insert_image(40, "a.cr3");
    f.set_stars(a, 2);
    let seen = std::sync::Mutex::new(Vec::<PathBuf>::new());
    let write = |w: &SidecarWrite| {
        seen.lock().unwrap().push(w.path.clone());
        production_write(w)
    };
    let outcome = write_one(&f.gate, &f.db, a, &write, &|| {}).unwrap();
    assert!(matches!(outcome, SidecarOutcome::Written), "{outcome:?}");
    let staged = seen.lock().unwrap()[0].clone();
    let name = staged.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with(".a.cr3.json.") && name.ends_with(".tmp"),
        "{name}"
    );
    assert!(!staged.exists(), "renamed into place");
    assert!(f.sidecar("a.cr3").is_some());
    assert_ne!(
        core_library::staging_path(&f.dir().join("a.cr3.json")),
        core_library::staging_path(&f.dir().join("a.cr3.json")),
        "every staging path is fresh"
    );
}

#[test]
fn concurrent_writes_in_one_directory_never_collide() {
    let f = Fix::new("staging-concurrent");
    let ids: Vec<i64> = (0..8u8)
        .map(|i| f.insert_image(50 + i, &format!("c{i}.cr3")))
        .collect();
    std::thread::scope(|scope| {
        for &id in &ids {
            let (gate, db) = (&f.gate, &f.db);
            scope.spawn(move || {
                for _ in 0..5 {
                    let outcome = write_one(gate, db, id, &production_write, &|| {}).unwrap();
                    assert!(matches!(outcome, SidecarOutcome::Written), "{outcome:?}");
                }
            });
        }
    });
    for i in 0..8u8 {
        assert!(f.sidecar(&format!("c{i}.cr3")).is_some());
    }
    let temps = std::fs::read_dir(f.dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(temps, 0, "no staging litter");
}

#[test]
fn stale_publication_and_failed_staging_leave_no_temp() {
    let f = Fix::new("staging-stale");
    let a = f.insert_image(60, "a.cr3");
    f.set_stars(a, 1);
    // Stale fence: the staged bytes exist when the gate moves, and are removed.
    let outcome = write_one(&f.gate, &f.db, a, &production_write, &|| {
        f.gate.replace_source(ImageId(a), f.key(61, "a.cr3"));
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Retry), "{outcome:?}");
    assert_no_publication(&f);
    // Catalog replacement (invalidate_all) is fenced the same way.
    let outcome = write_one(&f.gate, &f.db, a, &production_write, &|| {
        drop(f.gate.invalidate_all_guard());
    })
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Retry), "{outcome:?}");
    assert_no_publication(&f);
    // A staging write that dies half-way: its partial temp is removed. (Re-register the row's
    // real identity first: the replace above moved the gate to another key.)
    f.gate.replace_source(ImageId(a), f.key(60, "a.cr3"));
    let outcome = write_one(
        &f.gate,
        &f.db,
        a,
        &|w| {
            std::fs::write(&w.path, b"partial")?;
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
        },
        &|| {},
    )
    .unwrap();
    assert!(matches!(outcome, SidecarOutcome::Failed(_)), "{outcome:?}");
    assert_no_publication(&f);
}
