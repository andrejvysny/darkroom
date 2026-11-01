//! The per-image sidecar write decision, kept free of `AppHandle` so the identity rules are unit
//! testable against a real catalog; `sidecar_queue` only schedules when this runs.
//!
//! Three defects this module exists to rule out:
//! - An *unrelated* catalog mutation (the watcher inserting image B) must not cost image A its
//!   sidecar. Reading the gate's catalog-wide `sequence` up front did exactly that: any mutation made
//!   A's token uncallable and the id was dropped as "source identity stale".
//! - A row that is deleted, goes `missing`, or is repointed at other bytes mid-flight must not get a
//!   sidecar written at all.
//! - A filesystem failure (read-only volume, full disk) must not be reported as done: every outcome
//!   that is not `Written`/`Gone` returns the id to the dirty set, so the intent survives until the
//!   sidecar really matches the catalog.
//!
//! Lock order matches the rest of the app: publication gate before catalog. A catalog guard never
//! outlives a catalog read, so the filesystem write runs with it released (the fence is held).

use core_db::{rusqlite::Connection, Db};
use core_library::{LibError, SidecarWrite};
use core_model::identity::ImageId;
use core_runtime::publication::{PublicationGate, SourceKey};
use std::sync::Mutex;

const POISONED: &str = "sidecar catalog lock poisoned";

/// What one sidecar attempt decided. The queue maps this onto the dirty set: `Written`/`Gone` are
/// finished, everything else goes back for another pass.
#[derive(Debug)]
pub(crate) enum SidecarOutcome {
    /// File written and matches the catalog snapshot.
    Written,
    /// Row deleted / not `present` / repointed to another file: nothing to mirror any more.
    Gone,
    /// Identity or snapshot moved while we worked; re-snapshot and try again.
    Retry,
    /// Filesystem write failed; keep the intent and retry later. Carries the error text.
    Failed(String),
}

/// Write `image_id`'s sidecar iff the catalog still says what it said when the snapshot was gathered.
///
/// `before_publish` is a test seam run after the snapshot + capture and before the fenced re-read: it
/// models the worker being paused mid-write. Production passes `&|| {}`. `Err` is reserved for a
/// poisoned catalog lock — every other problem is an outcome.
pub(crate) fn write_one(
    gate: &PublicationGate,
    db: &Mutex<Db>,
    image_id: i64,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    before_publish: &dyn Fn(),
) -> Result<SidecarOutcome, String> {
    // Deliberately no catalog-wide `sequence` pre-read: the real check is the in-fence re-read below,
    // which compares *this* image's identity and snapshot, so an unrelated image cannot invalidate it.
    let (key, snapshot) = match read_state(db, image_id)? {
        State::Live(key, snapshot) => (key, snapshot),
        State::Gone => return Ok(SidecarOutcome::Gone),
        State::Unknown => return Ok(SidecarOutcome::Retry),
    };
    let Ok(token) = gate.capture(ImageId(image_id), key.clone()) else {
        return Ok(SidecarOutcome::Retry);
    };
    before_publish();
    let fenced = gate.with_current(&token, || -> Result<SidecarOutcome, String> {
        match decide(read_state(db, image_id)?, &key, &snapshot) {
            Decision::Gone => Ok(SidecarOutcome::Gone),
            Decision::Retry => Ok(SidecarOutcome::Retry),
            // `write` is filesystem IO: the catalog guard is released (it never left `read_state`),
            // only the fence is held — same shape as before this was extracted.
            Decision::Publish => Ok(match write(&snapshot) {
                Ok(()) => SidecarOutcome::Written,
                Err(error) => SidecarOutcome::Failed(error.to_string()),
            }),
        }
    });
    match fenced {
        Ok(outcome) => outcome,
        // The fence moved between capture and publish. Only a row that is really gone justifies
        // dropping the intent; anything else re-snapshots.
        Err(_) => Ok(if is_gone(db, image_id)? {
            SidecarOutcome::Gone
        } else {
            SidecarOutcome::Retry
        }),
    }
}

/// The production `write`: core-library's catalog-free atomic sidecar rename. Keeping the mapping
/// here lets the tests below exercise the same call the worker makes; the `io` variant is preserved
/// so a real `PermissionDenied` kind survives into `SidecarOutcome::Failed`.
pub(crate) fn production_write(snapshot: &SidecarWrite) -> std::io::Result<()> {
    match core_library::write_snapshot(snapshot) {
        Ok(()) => Ok(()),
        Err(LibError::Io(error)) => Err(error),
        Err(error) => Err(std::io::Error::other(error.to_string())),
    }
}

/// Log-safe summary of a write failure: the message with any path-shaped token masked. Same
/// no-user-data policy as `logging::safe_error` (which only ever logged a type name), but it keeps
/// the OS message that actually explains a read-only volume or a full disk.
pub(crate) fn safe_write_error(error: &str) -> String {
    error
        .split_whitespace()
        .map(|token| {
            if token.contains('/') || token.contains('\\') {
                "[path]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// What one catalog read said about an image.
enum State {
    /// Present row: its source identity plus the sidecar gathered from it (boxed so the variant
    /// stays small; the snapshot is the only bulk payload and it is read once per attempt).
    Live(SourceKey, Box<SidecarWrite>),
    /// No row, or not `present`: there is nothing to mirror.
    Gone,
    /// The catalog could not be read — undecidable, so the intent must not be dropped.
    Unknown,
}

/// In-fence verdict for a freshly read state against what we captured.
enum Decision {
    Publish,
    Gone,
    Retry,
}

/// Read identity + sidecar snapshot under a catalog guard that is dropped before returning.
fn read_state(db: &Mutex<Db>, image_id: i64) -> Result<State, String> {
    let db = db.lock().map_err(|_| POISONED.to_string())?;
    let row = match core_library::image_by_id(&db.conn, image_id) {
        Ok(Some(row)) => row,
        Ok(None) => return Ok(State::Gone),
        Err(_) => return Ok(State::Unknown),
    };
    match is_present(&db.conn, image_id) {
        Some(true) => {}
        // Row went `missing` between queueing and now — nothing to mirror right now. The outbox
        // intent is left queued (`complete_outbox` only deletes on `Gone`), so a relink
        // (`relink_missing_image`) or a watcher flip back to `present` gets it mirrored.
        Some(false) => return Ok(State::Gone),
        None => return Ok(State::Unknown),
    }
    match core_library::sidecar_snapshot(&db.conn, image_id) {
        Ok(Some(snapshot)) => Ok(State::Live(
            SourceKey {
                hash: row.content_hash,
                path: row.path,
            },
            Box::new(snapshot),
        )),
        Ok(None) => Ok(State::Gone),
        Err(_) => Ok(State::Unknown),
    }
}

fn decide(state: State, key: &SourceKey, snapshot: &SidecarWrite) -> Decision {
    match state {
        State::Gone => Decision::Gone,
        State::Unknown => Decision::Retry,
        State::Live(current_key, current) => {
            if &current_key != key {
                // Bytes or path moved under us: what we gathered describes a file that no longer
                // exists as far as this row is concerned.
                Decision::Gone
            } else if same_snapshot(&current, snapshot) {
                Decision::Publish
            } else {
                Decision::Retry
            }
        }
    }
}

/// Same destination and same sidecar content? `updated_at` is stamped at gather time, so two
/// snapshots of an unchanged image differ only there — normalize it before comparing.
fn same_snapshot(current: &SidecarWrite, captured: &SidecarWrite) -> bool {
    let mut latest = current.clone();
    latest.sidecar.updated_at = captured.sidecar.updated_at;
    latest.path == captured.path && latest.sidecar == captured.sidecar
}

/// Presence-only re-check for the post-`Stale` path: cheaper than a full snapshot, and the only
/// question there is whether the row still describes a file worth mirroring.
fn is_gone(db: &Mutex<Db>, image_id: i64) -> Result<bool, String> {
    let db = db.lock().map_err(|_| POISONED.to_string())?;
    match core_library::image_by_id(&db.conn, image_id) {
        Ok(None) => Ok(true),
        Ok(Some(_)) => Ok(match is_present(&db.conn, image_id) {
            Some(present) => !present,
            None => false,
        }),
        // A catalog we cannot read is not evidence that the image is gone.
        Err(_) => Ok(false),
    }
}

/// `Some(true)`/`Some(false)` for a row that is / is not `present`; `None` when the catalog could not
/// answer, which callers treat as "undecidable" rather than evidence the image is gone.
fn is_present(conn: &Connection, image_id: i64) -> Option<bool> {
    conn.query_row(
        "SELECT status = 'present' FROM images WHERE id = ?1",
        [image_id],
        |r| r.get::<_, bool>(0),
    )
    .ok()
}

/// Real-catalog tests for [`write_one`]. `pub(crate)` so `sidecar_queue`'s mapping tests can reuse
/// the [`Fix`] catalog instead of duplicating it.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use core_db::rusqlite::params;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(crate) struct Fix {
        dir: PathBuf,
        pub(crate) gate: PublicationGate,
        pub(crate) db: std::sync::Arc<Mutex<Db>>,
    }

    impl Fix {
        pub(crate) fn new(name: &str) -> Self {
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
                db: std::sync::Arc::new(Mutex::new(
                    Db::open(&self.dir.join("catalog.db")).unwrap(),
                )),
            }
        }

        /// Real file + `present` row on disk; `tag` is the content-hash byte. Returns the image id.
        pub(crate) fn insert_image(&self, tag: u8, filename: &str) -> i64 {
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

        pub(crate) fn sidecar(&self, filename: &str) -> Option<core_library::Sidecar> {
            core_library::sidecar::read_sidecar(&self.path(filename))
        }
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
        assert!(
            !called.load(Ordering::SeqCst),
            "a stale fence must not write unverified bytes"
        );
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
        assert!(matches!(outcome, SidecarOutcome::Gone));
        // Never there at all.
        let outcome = write_one(&f.gate, &f.db, 99_999, &recording(&called), &|| {}).unwrap();
        assert!(matches!(outcome, SidecarOutcome::Gone));
        assert!(
            !called.load(Ordering::SeqCst),
            "a gone image is never written"
        );
        assert!(f.sidecar("deleted.cr3").is_none());
        assert!(f.sidecar("missing.cr3").is_none());
    }

    #[test]
    fn replaced_bytes_while_paused_is_gone() {
        let f = Fix::new("replaced-bytes");
        let a = f.insert_image(5, "a.cr3");
        let called = AtomicBool::new(false);
        let outcome =
            write_one(&f.gate, &f.db, a, &recorded(&called), &|| f.repoint(a, 9)).unwrap();
        assert!(matches!(outcome, SidecarOutcome::Gone));
        assert!(
            !called.load(Ordering::SeqCst),
            "bytes gathered before the swap must never reach disk"
        );
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
        let outcome =
            write_one(&f.gate, &f.db, a, &recorded(&called), &|| f.set_stars(a, 5)).unwrap();
        assert!(matches!(outcome, SidecarOutcome::Retry));
        assert!(!called.load(Ordering::SeqCst));
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
        assert!(!called.load(Ordering::SeqCst));
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
        assert!(!called.load(Ordering::SeqCst));
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
}
