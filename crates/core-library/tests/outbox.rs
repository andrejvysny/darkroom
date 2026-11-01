//! The durable sidecar-mirror outbox (migration 026). Real on-disk catalog, not in-memory: half
//! of what the queue promises is that it survives a restart, which an in-memory DB cannot show.

use core_db::rusqlite::{params, Connection};
use core_db::Db;
use core_library::{
    complete_outbox, fail_outbox, get_edit, get_edit_with_version, mirrored_revision,
    pending_outbox, save_edit_with_intent, OutboxCompletion,
};
use std::path::PathBuf;

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("dr_outbox_{tag}_{}_{}", std::process::id(), n));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `images.content_hash` holds the 32 raw BLAKE3 bytes; the outbox hands out hex.
fn insert_image(conn: &Connection, hash: &[u8; 32], path: &str) -> i64 {
    conn.execute(
        "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                            capture_date, imported_at)
         VALUES (?1, 64, ?2, ?3, 'present', 1000, 0)",
        params![hash.to_vec(), path, "shot.cr3"],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn outbox_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM sidecar_outbox", [], |r| r.get(0))
        .unwrap()
}

const TIME: i64 = 1_700_000_000;

#[test]
fn a_save_bumps_the_revision_and_keeps_exactly_one_intent_per_image() {
    let scratch = Scratch::new("save");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0xab; 32], "/lib/shot.cr3");

    let revision = save_edit_with_intent(&db.conn, id, 5, r#"{"exposure":1}"#, TIME).unwrap();
    assert_eq!(revision, 1, "the first durable save is revision 1");
    assert_eq!(
        mirrored_revision(&db.conn, id).unwrap(),
        Some((1, 0)),
        "an unwritten sidecar is owed, not mirrored"
    );

    let queued = pending_outbox(&db.conn, i64::MAX, None, 10).unwrap();
    assert_eq!(queued.len(), 1, "one intent per image");
    assert_eq!(queued[0].image_id, id);
    assert_eq!(queued[0].edit_revision, 1);
    assert_eq!(queued[0].content_hash_hex, "ab".repeat(32));
    assert_eq!(queued[0].path, "/lib/shot.cr3");
    assert_eq!(queued[0].attempts, 0);
    assert_eq!(queued[0].last_error, None);

    // A second save rewrites the single intent rather than queueing a second write: only the newest
    // params matter, and two rows would race each other for the same sidecar file.
    let revision = save_edit_with_intent(&db.conn, id, 6, r#"{"exposure":2}"#, TIME).unwrap();
    assert_eq!(revision, 2);
    let queued = pending_outbox(&db.conn, i64::MAX, None, 10).unwrap();
    assert_eq!(queued.len(), 1, "still one row, replaced not appended");
    assert_eq!(queued[0].edit_revision, 2);
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((2, 0)));

    // The save still behaves like `set_edit`: same columns, same monotonic `updated_at` rule.
    assert_eq!(
        get_edit(&db.conn, id).unwrap().as_deref(),
        Some(r#"{"exposure":2}"#)
    );
    assert_eq!(
        get_edit_with_version(&db.conn, id).unwrap().unwrap().1,
        TIME + 1,
        "the second save landed in the same clock tick, so the +1 rule applied as in `set_edit`"
    );
    // Clock regression still forces a distinct cache revision.
    save_edit_with_intent(&db.conn, id, 6, r#"{"exposure":3}"#, TIME - 60).unwrap();
    assert_eq!(
        get_edit_with_version(&db.conn, id).unwrap().unwrap().1,
        TIME + 2
    );
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((3, 0)));
}

#[test]
fn a_superseded_completion_marks_nothing_and_the_current_one_completes() {
    let scratch = Scratch::new("supersede");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0xcd; 32], "/lib/shot.cr3");

    save_edit_with_intent(&db.conn, id, 5, r#"{"exposure":1}"#, TIME).unwrap();
    let stale = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    save_edit_with_intent(&db.conn, id, 5, r#"{"exposure":2}"#, TIME + 1).unwrap();
    let current = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(current.edit_revision, 2);

    // The host finished writing the OLD params after the user saved again: that success proves
    // nothing about revision 2, so it must not clear the queue.
    assert_eq!(
        complete_outbox(&db.conn, &stale).unwrap(),
        OutboxCompletion::Superseded
    );
    let queued = pending_outbox(&db.conn, i64::MAX, None, 10).unwrap();
    assert_eq!(queued.len(), 1, "the newer intent survives");
    assert_eq!(queued[0].edit_revision, 2);
    assert_eq!(
        mirrored_revision(&db.conn, id).unwrap(),
        Some((2, 0)),
        "mirrored must not advance on a superseded write"
    );

    assert_eq!(
        complete_outbox(&db.conn, &current).unwrap(),
        OutboxCompletion::Completed
    );
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((2, 2)));
    assert!(pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .is_empty());
    assert_eq!(outbox_count(&db.conn), 0);

    // Reporting the same success twice is harmless.
    assert_eq!(
        complete_outbox(&db.conn, &current).unwrap(),
        OutboxCompletion::Completed
    );
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((2, 2)));
}

#[test]
fn a_moved_or_replaced_relinkable_source_requeues_the_intent_at_the_new_identity() {
    let scratch = Scratch::new("moved");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0xef; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    // An older failed attempt on the old path: relinking must reset the retry pacing, because the
    // old path's failures say nothing about the new file.
    fail_outbox(&db.conn, id, 1, "permission denied", 0).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(item.attempts, 1);

    // Renamed on disk (e.g. the watcher adopted the file's real name) while the write was in
    // flight: the row still exists and is present, and still owes revision 1. Dropping the intent
    // would strand it — relink never re-queues — so it is re-pointed at the CURRENT identity.
    db.conn
        .execute(
            "UPDATE images SET path = ?2 WHERE id = ?1",
            params![id, "/lib/renamed.cr3"],
        )
        .unwrap();
    assert_eq!(
        complete_outbox(&db.conn, &item).unwrap(),
        OutboxCompletion::Requeued
    );
    let requeued = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .into_iter()
        .find(|i| i.image_id == id)
        .expect("the intent survives relinked, not deleted");
    assert_eq!(
        requeued.path, "/lib/renamed.cr3",
        "re-pointed at the new path"
    );
    assert_eq!(
        requeued.content_hash_hex, item.content_hash_hex,
        "the unchanged body keeps its hash"
    );
    assert_eq!(requeued.edit_revision, 1, "the revision still owed is kept");
    assert_eq!(
        requeued.attempts, 0,
        "the new file starts with clean retry pacing"
    );
    assert_eq!(requeued.last_error, None);
    assert_eq!(
        mirrored_revision(&db.conn, id).unwrap(),
        Some((1, 0)),
        "nothing on disk was mirrored"
    );

    // The caller retries now: the same intent, re-run against the current identity, settles.
    assert_eq!(
        complete_outbox(&db.conn, &requeued).unwrap(),
        OutboxCompletion::Completed
    );
    assert!(pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .is_empty());
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((1, 1)));

    // Same for a replaced body (re-copied card): the hash no longer matches the intent, but the
    // present row still owes the write, so it re-queues at the new hash.
    let revision = save_edit_with_intent(&db.conn, id, 5, "{}", TIME + 1).unwrap();
    assert_eq!(revision, 2);
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    db.conn
        .execute(
            "UPDATE images SET content_hash = ?2 WHERE id = ?1",
            params![id, vec![0x22u8; 32]],
        )
        .unwrap();
    assert_eq!(
        complete_outbox(&db.conn, &item).unwrap(),
        OutboxCompletion::Requeued
    );
    let requeued = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(requeued.content_hash_hex, "22".repeat(32));
    assert_eq!(
        requeued.path, "/lib/renamed.cr3",
        "the path is refreshed too"
    );
    assert_eq!(requeued.edit_revision, 2);
    assert_eq!(
        mirrored_revision(&db.conn, id).unwrap(),
        Some((2, 1)),
        "the requeue never marks anything mirrored"
    );
    assert_eq!(
        complete_outbox(&db.conn, &requeued).unwrap(),
        OutboxCompletion::Completed
    );
}

#[test]
fn a_moved_source_that_is_not_present_keeps_the_intent_queued() {
    let scratch = Scratch::new("moved_missing");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0x5c; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);

    // Moved AND its on-disk original vanished (watcher flagged `missing`) mid-write: nothing on
    // disk mirrors right now, but the row may come back (relink / remount). Only a gone row or a
    // gone edit ever deletes an intent.
    db.conn
        .execute(
            "UPDATE images SET path = ?2, status = 'missing' WHERE id = ?1",
            params![id, "/lib/elsewhere.cr3"],
        )
        .unwrap();
    assert_eq!(
        complete_outbox(&db.conn, &item).unwrap(),
        OutboxCompletion::SourceChanged
    );
    let kept = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .into_iter()
        .find(|i| i.image_id == id)
        .expect("a missing row keeps its intent for the relink");
    assert_eq!(
        (kept.path.as_str(), kept.edit_revision),
        ("/lib/shot.cr3", 1),
        "the queued intent is left exactly as it was"
    );

    // The relink lands: same hash at a new path, `present` again. The queued intent (old path)
    // completes the normal way — the write re-runs, `complete_outbox` requeues at the relinked
    // identity, and the retry settles it.
    db.conn
        .execute(
            "UPDATE images SET path = ?2, status = 'present' WHERE id = ?1",
            params![id, "/lib/elsewhere.cr3"],
        )
        .unwrap();
    let retried = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(
        complete_outbox(&db.conn, &retried).unwrap(),
        OutboxCompletion::Requeued
    );
    let relinked = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(relinked.path, "/lib/elsewhere.cr3");
    assert_eq!(
        complete_outbox(&db.conn, &relinked).unwrap(),
        OutboxCompletion::Completed
    );
    assert!(pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn a_deleted_image_or_edit_takes_the_intent_with_it() {
    let scratch = Scratch::new("gone");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0x99; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);

    db.conn
        .execute("DELETE FROM images WHERE id = ?1", params![id])
        .unwrap();
    assert_eq!(
        outbox_count(&db.conn),
        0,
        "deleting the image cascades the intent away"
    );
    assert_eq!(
        complete_outbox(&db.conn, &item).unwrap(),
        OutboxCompletion::Gone
    );
    assert_eq!(
        mirrored_revision(&db.conn, id).unwrap(),
        None,
        "the edit row cascaded with the image"
    );
}

#[test]
fn a_deleted_edit_drops_the_intent_it_could_not_keep() {
    let scratch = Scratch::new("edit_gone");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0x5a; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);

    // `edits` has no cascade from `sidecar_outbox` — "reset develop params" deletes the edit row and
    // leaves the intent behind. Completing it must clear it, not mark a nonexistent edit mirrored.
    db.conn
        .execute("DELETE FROM edits WHERE image_id = ?1", params![id])
        .unwrap();
    assert_eq!(
        outbox_count(&db.conn),
        1,
        "the intent outlives the edit row"
    );
    assert_eq!(
        complete_outbox(&db.conn, &item).unwrap(),
        OutboxCompletion::Gone
    );
    assert_eq!(outbox_count(&db.conn), 0, "the dead intent is dropped");
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), None);
}

#[test]
fn a_failed_write_is_counted_and_survives_a_restart() {
    let scratch = Scratch::new("retry");
    let path = scratch.0.join("catalog.db");
    let db = Db::open(&path).unwrap();
    let id = insert_image(&db.conn, &[0x77; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();

    fail_outbox(&db.conn, id, 1, "disk full", 0).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(item.attempts, 1);
    assert_eq!(item.last_error.as_deref(), Some("disk full"));

    fail_outbox(&db.conn, id, 1, "read-only volume", 0).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(
        item.attempts, 2,
        "the retry loop needs the attempt count to back off"
    );
    assert_eq!(item.last_error.as_deref(), Some("read-only volume"));

    // An attempt attributed to a revision that is no longer queued changes nothing.
    fail_outbox(&db.conn, id, 99, "stale", 0).unwrap();
    assert_eq!(
        pending_outbox(&db.conn, i64::MAX, None, 10).unwrap()[0].attempts,
        2,
        "only the attempted intent is touched"
    );

    // The crash test: drop the connection (quit), open it again (restart). The queue is still there.
    drop(db);
    let db = Db::open(&path).unwrap();
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    assert_eq!(item.image_id, id);
    assert_eq!(item.edit_revision, 1);
    assert_eq!(item.attempts, 2);
    assert_eq!(item.last_error.as_deref(), Some("read-only volume"));
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((1, 0)));

    // The eventual success closes it out.
    assert_eq!(
        complete_outbox(&db.conn, &item).unwrap(),
        OutboxCompletion::Completed
    );
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((1, 1)));
    assert!(pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn out_of_order_completions_never_rewind_the_mirror() {
    let scratch = Scratch::new("order");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0x33; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    let first = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME + 1).unwrap();
    let second = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);

    // The writer for revision 2 reports before the one for revision 1 (separate worker tasks).
    assert_eq!(
        complete_outbox(&db.conn, &second).unwrap(),
        OutboxCompletion::Completed
    );
    assert_eq!(mirrored_revision(&db.conn, id).unwrap(), Some((2, 2)));
    assert_eq!(
        complete_outbox(&db.conn, &first).unwrap(),
        OutboxCompletion::Superseded
    );
    assert_eq!(
        mirrored_revision(&db.conn, id).unwrap(),
        Some((2, 2)),
        "a late completion for an older revision must not rewind it"
    );
}

#[test]
fn the_queue_is_oldest_first_and_respects_the_limit() {
    let scratch = Scratch::new("order_limit");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let mut ids = Vec::new();
    for tag in 1..=3u8 {
        let id = insert_image(&db.conn, &[tag; 32], &format!("/lib/{tag}.cr3"));
        save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
        ids.push(id);
    }
    let queued = pending_outbox(&db.conn, i64::MAX, None, 10).unwrap();
    assert_eq!(queued.len(), 3);
    // Same second, so the tie-break on image_id gives a stable, starvation-free order.
    assert_eq!(
        queued.iter().map(|i| i.image_id).collect::<Vec<_>>(),
        ids,
        "oldest intent first"
    );
    assert_eq!(
        pending_outbox(&db.conn, i64::MAX, None, 2).unwrap().len(),
        2
    );
    assert!(pending_outbox(&db.conn, i64::MAX, None, 0)
        .unwrap()
        .is_empty());
}

#[test]
fn saving_for_an_unknown_image_writes_nothing() {
    let scratch = Scratch::new("missing");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();

    let err = save_edit_with_intent(&db.conn, 99_999, 5, "{}", TIME)
        .expect_err("an image that does not exist cannot have a sidecar");
    assert!(err.to_string().contains("99999"), "name the image: {err}");
    assert_eq!(outbox_count(&db.conn), 0);
    assert_eq!(mirrored_revision(&db.conn, 99_999).unwrap(), None);
    let has_edit: bool = db
        .conn
        .prepare("SELECT 1 FROM edits WHERE image_id = ?1")
        .unwrap()
        .exists(params![99_999])
        .unwrap();
    assert!(
        !has_edit,
        "the transaction must roll back, leaving no edit row"
    );
}

#[test]
fn an_intent_and_its_edit_commit_together() {
    // The crash-safety claim in one assertion: an edit row whose revision is ahead of the mirror is
    // ALWAYS paired with an intent row, which is what makes a restart able to retry the write.
    let scratch = Scratch::new("atomic");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[0x42; 32], "/lib/shot.cr3");
    for _ in 0..3 {
        save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    }
    let (revision, mirrored) = mirrored_revision(&db.conn, id).unwrap().unwrap();
    assert_eq!((revision, mirrored), (3, 0));
    assert_eq!(outbox_count(&db.conn), 1);

    // And the reverse: a completed mirror never leaves a row behind.
    let item = pending_outbox(&db.conn, i64::MAX, None, 10)
        .unwrap()
        .remove(0);
    complete_outbox(&db.conn, &item).unwrap();
    assert_eq!(
        db.conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sidecar_outbox) \
                 + (SELECT COUNT(*) FROM edits WHERE revision <> mirrored_revision)",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0,
        "no owed write is unrecorded, and no recorded intent is unowed"
    );
}
#[test]
fn due_filter_keyset_paging_and_schedule_controls() {
    use core_library::{defer_outbox, next_outbox_due, reset_outbox_schedule};
    let scratch = Scratch::new("schedule");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let ids: Vec<i64> = (0..5u8)
        .map(|i| insert_image(&db.conn, &[i + 1; 32], &format!("/lib/{i}.cr3")))
        .collect();
    for &id in &ids {
        save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    }
    assert_eq!(next_outbox_due(&db.conn).unwrap(), Some(0));

    // Paging by keyset walks every due row exactly once, whatever the page size.
    let mut seen = Vec::new();
    let mut after = None;
    loop {
        let page = core_library::pending_outbox_keyed(&db.conn, 1_000, after, 2).unwrap();
        if page.is_empty() {
            break;
        }
        after = page.last().map(|(_, cursor)| *cursor);
        seen.extend(page.iter().map(|(item, _)| item.image_id));
    }
    assert_eq!(seen.len(), 5);
    let mut sorted = seen.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 5, "no row repeated: {seen:?}");

    // A backed-off row is invisible until its time; the others stay due. `defer` charges no attempt.
    fail_outbox(&db.conn, ids[0], 1, "boom", 5_000).unwrap();
    defer_outbox(&db.conn, ids[1], 1, 7_000, Some("not present")).unwrap();
    let due_at = |now| -> Vec<i64> {
        pending_outbox(&db.conn, now, None, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.image_id)
            .collect()
    };
    assert_eq!(due_at(1_000), ids[2..].to_vec());
    assert_eq!(next_outbox_due(&db.conn).unwrap(), Some(0));
    assert_eq!(due_at(5_000).len(), 4);
    assert_eq!(due_at(7_000).len(), 5);
    let failed = pending_outbox(&db.conn, 5_000, None, 10).unwrap();
    let failed = failed.iter().find(|r| r.image_id == ids[0]).unwrap();
    assert_eq!((failed.attempts, failed.next_attempt_at), (1, 5_000));
    let deferred = pending_outbox(&db.conn, 7_000, None, 10).unwrap();
    let deferred = deferred.iter().find(|r| r.image_id == ids[1]).unwrap();
    assert_eq!(deferred.attempts, 0, "defer is not a failed attempt");
    assert_eq!(deferred.last_error.as_deref(), Some("not present"));

    // A stale revision touches nothing.
    defer_outbox(&db.conn, ids[2], 99, 9_000, None).unwrap();
    assert_eq!(due_at(1_000), ids[2..].to_vec());

    // Reset one, then all; attempts and last_error are preserved.
    assert_eq!(reset_outbox_schedule(&db.conn, Some(ids[0])).unwrap(), 1);
    assert_eq!(due_at(0).len(), 4);
    reset_outbox_schedule(&db.conn, None).unwrap();
    assert_eq!(due_at(0).len(), 5);
    let kept = pending_outbox(&db.conn, 0, None, 10).unwrap();
    let kept = kept.iter().find(|r| r.image_id == ids[0]).unwrap();
    assert_eq!(
        (kept.attempts, kept.last_error.as_deref()),
        (1, Some("boom"))
    );
}

#[test]
fn a_superseding_save_resets_the_inherited_backoff() {
    let scratch = Scratch::new("supersede-schedule");
    let db = Db::open(&scratch.0.join("catalog.db")).unwrap();
    let id = insert_image(&db.conn, &[3; 32], "/lib/shot.cr3");
    save_edit_with_intent(&db.conn, id, 5, "{}", TIME).unwrap();
    fail_outbox(&db.conn, id, 1, "disk full", 9_999_999).unwrap();
    assert!(pending_outbox(&db.conn, 1_000, None, 10)
        .unwrap()
        .is_empty());

    save_edit_with_intent(&db.conn, id, 5, "{}", TIME + 1).unwrap();
    let item = pending_outbox(&db.conn, 1_000, None, 10).unwrap().remove(0);
    assert_eq!(
        (item.edit_revision, item.attempts, item.next_attempt_at),
        (2, 0, 0)
    );
    assert_eq!(item.last_error, None);
}
