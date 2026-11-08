use super::{apply_status_changes, AppliedStatusChange, PublicationFence};
use core_model::identity::ImageId;
use core_runtime::publication::{PublicationGate, SourceKey};
use std::sync::{mpsc, Arc, Mutex};

fn key(tag: char, path: &str) -> SourceKey {
    SourceKey {
        hash: tag.to_string().repeat(64),
        path: path.into(),
    }
}

#[test]
fn per_source_retire_keeps_unrelated_tokens_valid() {
    let gate = PublicationGate::default();
    let token1 = gate.capture(ImageId(1), key('a', "/lib/a.cr3")).unwrap();
    let token2 = gate.capture(ImageId(2), key('b', "/lib/b.cr3")).unwrap();
    let epoch_before = gate.epoch();
    // The watcher path mutates identities through the serialized source guard.
    gate.source_guard().retire(ImageId(1));
    assert!(gate.with_current(&token1, || ()).is_err());
    assert!(gate.with_current(&token2, || ()).is_ok());
    assert_eq!(gate.epoch(), epoch_before);
    // A retired id re-registers with a fresh key before tokens can be captured again.
    gate.replace_source(ImageId(1), key('a', "/lib/a.cr3"));
    assert!(gate.capture(ImageId(1), key('a', "/lib/a.cr3")).is_ok());
}

#[test]
fn watcher_status_commit_updates_only_changed_sources() {
    let gate = PublicationGate::default();
    let missing_token = gate.capture(ImageId(1), key('m', "/lib/gone.cr3")).unwrap();
    let kept_token = gate.capture(ImageId(2), key('k', "/lib/kept.cr3")).unwrap();
    let epoch_before = gate.epoch();
    let mut guard = gate.source_guard();
    apply_status_changes(
        &mut guard,
        &[
            // Went missing on disk: retire, no key.
            AppliedStatusChange { id: 1, key: None },
            // Came back: carries the identity read from the committed transaction.
            AppliedStatusChange {
                id: 3,
                key: Some(key('r', "/lib/back.cr3")),
            },
        ],
    );
    drop(guard);
    assert!(gate.with_current(&missing_token, || ()).is_err());
    // An untouched image's token survives; the epoch did not move, so jobs keep their
    // supersession baseline.
    assert!(gate.with_current(&kept_token, || ()).is_ok());
    assert_eq!(gate.epoch(), epoch_before);
    // The re-appeared row is immediately capturable with its committed identity.
    assert!(gate.capture(ImageId(3), key('r', "/lib/back.cr3")).is_ok());
}

#[test]
fn supported_paths_match_watcher_rules() {
    assert!(super::supported_path(std::path::Path::new("image.CR3")));
    assert!(!super::supported_path(std::path::Path::new("._image.CR3")));
    assert!(!super::supported_path(std::path::Path::new("image.txt")));
}

#[test]
fn retry_wake_coalesces_without_blocking() {
    let fence = PublicationFence::default();
    let (sender, receiver) = mpsc::sync_channel(1);
    *fence.wake.lock().unwrap() = Some(sender.clone());
    fence.request_retry();
    sender.try_send(()).unwrap();
    assert!(matches!(
        sender.try_send(()),
        Err(mpsc::TrySendError::Full(()))
    ));
    assert!(fence.take_retry_if_idle());
    receiver.try_recv().unwrap();
    assert!(receiver.try_recv().is_err());
}

#[test]
fn publication_completed_during_decode_invalidates_snapshot_and_retries() {
    let fence = PublicationFence::default();
    let db = Mutex::new(());
    let generation = fence.snapshot(&db.lock().unwrap()).unwrap();
    // The watcher owns decoded bytes from before a whole import/export guard lifetime.
    fence.begin(&db);
    fence.end(&db);
    assert!(!fence.is_active());
    assert!(!fence.is_current(&db.lock().unwrap(), generation));
    // No guard remains to drain this late request, so the watcher must schedule it itself.
    fence.request_retry();
    assert!(fence.take_retry_if_idle());
    assert!(!fence.take_retry_if_idle());
}

#[test]
fn overlapping_publications_defer_retry_until_the_last_finishes() {
    let fence = PublicationFence::default();
    let db = Mutex::new(());
    let generation = fence.snapshot(&db.lock().unwrap()).unwrap();
    fence.begin(&db);
    fence.begin(&db);
    fence.request_retry();
    fence.end(&db);
    assert!(fence.snapshot(&db.lock().unwrap()).is_none());
    assert!(!fence.take_retry_if_idle());
    fence.end(&db);
    assert!(!fence.is_current(&db.lock().unwrap(), generation));
    assert!(fence.take_retry_if_idle());
}

#[test]
fn publication_start_waits_for_watcher_check_and_commit_lock() {
    let fence = Arc::new(PublicationFence::default());
    let db = Arc::new(Mutex::new(()));
    let insertion = db.lock().unwrap();
    let generation = fence.snapshot(&insertion).unwrap();
    let (attempt_tx, attempt_rx) = mpsc::channel();
    let (started_tx, started_rx) = mpsc::channel();
    let writer = {
        let fence = Arc::clone(&fence);
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            attempt_tx.send(()).unwrap();
            fence.begin(&db);
            started_tx.send(()).unwrap();
        })
    };
    attempt_rx.recv().unwrap();
    // The real begin helper shares the insertion mutex: it cannot cross this check/commit.
    assert!(started_rx.try_recv().is_err());
    assert!(fence.is_current(&insertion, generation));
    drop(insertion);
    started_rx.recv().unwrap();
    writer.join().unwrap();
    assert!(!fence.is_current(&db.lock().unwrap(), generation));
    fence.end(&db);
}

/// The watcher's fenced commit (`commit_indexed`, extracted from `index_new` precisely so it can
/// run without an `AppHandle`) must register a fresh insert's identity under the held gate: SQLite
/// reuses the rowid of the newest deleted row, and an unregistered reuse stays Stale until restart.
#[test]
fn watcher_insert_registers_reused_id() {
    use super::{commit_indexed, IndexCommit};
    use core_library::{LibError, ProcessedImage};
    use core_raw::RawMeta;

    let mut db = core_db::Db::open_in_memory().unwrap();
    db.conn
        .execute(
            "INSERT INTO folders(id, path, is_watched, added_at) VALUES (1, '/lib', 1, 0)",
            [],
        )
        .unwrap();
    // Anchor row 4 + the doomed row 5: deleting row 5 makes the next insert reuse id 5.
    for id in [4i64, 5] {
        db.conn.execute(
            "INSERT INTO images(id, content_hash, file_size, path, original_filename, imported_at)
             VALUES (?1, ?2, 1, ?3, 'old.cr3', 0)",
            core_db::rusqlite::params![id, vec![id as u8; 32], format!("/lib/old{id}.cr3")],
        )
        .unwrap();
    }

    let gate = PublicationGate::default();
    let old_token = gate
        .capture(ImageId(5), key('o', "/lib/old5.cr3"))
        .expect("row 5 starts registered");
    // The delete flow retires the id, then SQLite will hand id 5 to the next insert.
    gate.source_guard().retire(ImageId(5));
    db.conn
        .execute("DELETE FROM images WHERE id = 5", [])
        .unwrap();
    assert!(
        gate.capture(ImageId(5), key('n', "/lib/new.cr3")).is_err(),
        "a retired id refuses captures until the insert registers it"
    );

    let processed = ProcessedImage {
        content_hash: [0x2au8; 32],
        content_hash_hex: "2a".repeat(32),
        file_size: 3,
        path: "/lib/new.cr3".into(),
        original_filename: "new.cr3".into(),
        meta: RawMeta::default(),
        width: 8,
        height: 8,
        capture_fingerprint: None,
        format: "raw",
    };
    let processed: Result<ProcessedImage, LibError> = Ok(processed);
    let mut guard = gate.source_guard();
    let outcome = commit_indexed(
        &mut guard,
        &mut db.conn,
        1,
        0,
        std::path::Path::new("/lib/new.cr3"),
        &|| false,
        &processed,
    );
    drop(guard);
    assert!(
        matches!(outcome, IndexCommit::Inserted),
        "row must be inserted"
    );

    let new_id: i64 = db
        .conn
        .query_row(
            "SELECT id FROM images WHERE path = '/lib/new.cr3'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(new_id, 5, "the deleted rowid was reused");

    // The committed identity is immediately publishable — no restart, no whole-gate reset.
    let token = gate
        .capture(
            ImageId(5),
            SourceKey {
                hash: "2a".repeat(32),
                path: "/lib/new.cr3".into(),
            },
        )
        .expect("reused id registered by the insert is capturable");
    assert!(gate.with_current(&token, || ()).is_ok());
    // The deleted row's token and key stay dead.
    assert!(gate.with_current(&old_token, || ()).is_err());
    assert!(gate.capture(ImageId(5), key('o', "/lib/old5.cr3")).is_err());
}

/// A `missing → present` reconcile whose identity read fails must abort the whole transaction
/// uncommitted. Committing with a `None` key would retire the id while its row is `present`;
/// `capture` then refuses that id forever and reconcile never re-fires (both sides say present),
/// so the image stays unpublishable for the whole run. The rollback keeps it `missing` for the
/// next pass. `apply_status_updates` is the `AppHandle`-free seam, like `commit_indexed`.
#[test]
fn unreadable_identity_on_present_transition_rolls_back_the_pass() {
    use super::{apply_status_updates, identity_of};

    let db = core_db::Db::open_in_memory().unwrap();
    db.conn
        .execute(
            "INSERT INTO images(id, content_hash, file_size, path, original_filename, status,
                                imported_at)
             VALUES (7, ?1, 1, '/lib/shot.cr3', 'shot.cr3', 'missing', 0)",
            core_db::rusqlite::params![vec![0x77u8; 32]],
        )
        .unwrap();
    let changes = [(
        7i64,
        "/lib/shot.cr3".to_string(),
        "missing".to_string(),
        "present".to_string(),
    )];

    // The real identity read would succeed here; simulate the failing one (`image_by_id` erroring
    // or returning None for a row the UPDATE just matched).
    let tx = db.conn.unchecked_transaction().unwrap();
    assert!(
        identity_of(&tx, 7).is_some(),
        "fixture row carries a readable identity"
    );
    assert!(
        apply_status_updates(&tx, &changes, &|| false, &|_tx, _id| None).is_none(),
        "an unreadable present identity must abort the pass"
    );
    drop(tx); // uncommitted → rollback, exactly what `reconcile_status` relies on.

    let status: String = db
        .conn
        .query_row("SELECT status FROM images WHERE id = 7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        status, "missing",
        "the status update must never commit; the next watcher pass retries"
    );

    // A shutdown request aborts the same way, before any row is written.
    let tx = db.conn.unchecked_transaction().unwrap();
    assert!(
        apply_status_updates(&tx, &changes, &|| true, &|tx, id| identity_of(tx, id)).is_none(),
        "stopping mid-loop aborts the pass"
    );
    drop(tx);
    let status: String = db
        .conn
        .query_row("SELECT status FROM images WHERE id = 7", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "missing", "a stopped pass writes nothing either");
}

/// The happy path through the same seam: a `present` transition carries the row's committed
/// path+hash (so the gate registers it and the id stays publishable), and a `missing` transition
/// retires its id.
#[test]
fn applied_status_changes_carry_committed_identities() {
    use super::{apply_status_updates, identity_of};

    let db = core_db::Db::open_in_memory().unwrap();
    for (id, tag, path, status) in [
        (1i64, 0xaau8, "/lib/back.cr3", "missing"),
        (2, 0xbb, "/lib/leaving.cr3", "present"),
    ] {
        db.conn
            .execute(
                "INSERT INTO images(id, content_hash, file_size, path, original_filename, status,
                                    imported_at)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, 0)",
                core_db::rusqlite::params![
                    id,
                    vec![tag; 32],
                    path,
                    path.rsplit('/').next().unwrap(),
                    status
                ],
            )
            .unwrap();
    }
    let changes = vec![
        (
            1i64,
            "/lib/back.cr3".to_string(),
            "missing".to_string(),
            "present".to_string(),
        ),
        (
            2,
            "/lib/leaving.cr3".to_string(),
            "present".to_string(),
            "missing".to_string(),
        ),
        // A stale candidate: the row does not exist (deleted since the snapshot), so the guarded
        // UPDATE matches 0 and contributes no applied change.
        (
            9,
            "/lib/gone.cr3".to_string(),
            "missing".to_string(),
            "present".to_string(),
        ),
    ];

    let tx = db.conn.unchecked_transaction().unwrap();
    let applied = apply_status_updates(&tx, &changes, &|| false, &|tx, id| identity_of(tx, id))
        .expect("all identities readable");
    assert_eq!(
        applied.len(),
        2,
        "the mismatched-status candidate updated nothing"
    );
    let back = applied.iter().find(|a| a.id == 1).unwrap();
    assert_eq!(
        back.key,
        Some(SourceKey {
            hash: "aa".repeat(32),
            path: "/lib/back.cr3".into(),
        }),
        "a present transition registers the row's committed identity"
    );
    let leaving = applied.iter().find(|a| a.id == 2).unwrap();
    assert_eq!(leaving.key, None, "a missing transition retires the id");
    tx.commit().unwrap();
}
