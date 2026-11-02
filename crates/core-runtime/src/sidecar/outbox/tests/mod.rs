use super::*;
use crate::sidecar::write::tests::Fix;
use core_library::{
    mirrored_revision, pending_outbox, reset_outbox_schedule, save_edit_with_intent,
};
use std::sync::atomic::{AtomicI64, AtomicUsize};

const PARAMS: &str = "{\"exposure\":0.5}";

/// Test clock: time only moves when the test says so.
struct FakeClock(AtomicI64);

impl FakeClock {
    fn new() -> Self {
        Self(AtomicI64::new(1_000_000))
    }
    fn advance(&self, d: Duration) {
        self.0.fetch_add(d.as_millis() as i64, Ordering::SeqCst);
    }
}

impl OutboxClock for FakeClock {
    fn now_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

fn never() -> bool {
    false
}

fn drain(
    f: &Fix,
    write: &dyn Fn(&SidecarWrite) -> std::io::Result<()>,
    c: &FakeClock,
) -> DrainReport {
    drain_once(&f.gate, &f.db, write, c, &never).unwrap()
}

/// The command's durable half: commit an edit plus its mirror intent.
fn save(f: &Fix, id: i64) -> i64 {
    let db = f.db.lock().unwrap();
    save_edit_with_intent(&db.conn, id, 7, PARAMS, core_library::now_epoch()).unwrap()
}

fn owed(f: &Fix) -> Vec<core_library::OutboxItem> {
    pending_outbox(&f.db.lock().unwrap().conn, i64::MAX, None, 1024).unwrap()
}

fn row(f: &Fix, id: i64) -> core_library::OutboxItem {
    owed(f)
        .into_iter()
        .find(|r| r.image_id == id)
        .expect("intent still queued")
}

fn set_status(f: &Fix, id: i64, status: &str) {
    f.db.lock()
        .unwrap()
        .conn
        .execute(
            "UPDATE images SET status = ?2 WHERE id = ?1",
            core_db::rusqlite::params![id, status],
        )
        .unwrap();
}

fn denied(_: &SidecarWrite) -> std::io::Result<()> {
    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
}

/// No staging litter anywhere in the fixture directory.
fn no_temp_files(f: &Fix) -> bool {
    std::fs::read_dir(f.dir())
        .unwrap()
        .filter_map(Result::ok)
        .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp"))
}

#[test]
fn a_saved_edit_is_mirrored_and_the_intent_settles() {
    let f = Fix::new("outbox-mirror");
    let a = f.insert_image(1, "a.cr3");
    let revision = save(&f, a);

    let report = drain(&f, &production_write, &FakeClock::new());
    assert_eq!(report.attempted, 1);
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert_eq!(report.next_due, None, "a settled intent owes nothing");

    let sidecar = f.sidecar("a.cr3").expect("sidecar written beside the RAW");
    assert_eq!(
        sidecar.develop,
        Some(serde_json::from_str::<serde_json::Value>(PARAMS).unwrap()),
        "the sidecar carries the params the catalog just committed"
    );
    assert_eq!(sidecar.process_version, Some(7));
    assert!(owed(&f).is_empty(), "the intent is gone once mirrored");
    assert!(no_temp_files(&f), "the staged temp was renamed away");
    assert_eq!(
        mirrored_revision(&f.db.lock().unwrap().conn, a).unwrap(),
        Some((revision, revision)),
        "mirrored == revision is the durable proof"
    );
}

#[test]
fn a_failed_write_keeps_the_intent_with_one_attempt() {
    let f = Fix::new("outbox-failure");
    let a = f.insert_image(2, "a.cr3");
    let revision = save(&f, a);
    let clock = FakeClock::new();

    let report = drain(&f, &denied, &clock);
    assert_eq!(report.kept, 1, "{report:?}");
    assert!(
        report.next_due.is_some_and(|due| due > clock.now_ms()),
        "a kept intent is timed in the future, not spun on: {report:?}"
    );
    assert!(
        report
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("denied")),
        "{report:?}"
    );

    let rows = owed(&f);
    assert_eq!(rows.len(), 1, "the intent survives the failed write");
    assert_eq!(rows[0].image_id, a);
    assert_eq!(rows[0].edit_revision, revision);
    assert_eq!(
        rows[0].attempts, 1,
        "the attempt is recorded, not swallowed"
    );
    assert!(rows[0]
        .last_error
        .as_deref()
        .is_some_and(|e| e.contains("denied")));
    assert_eq!(
        mirrored_revision(&f.db.lock().unwrap().conn, a).unwrap(),
        Some((revision, 0)),
        "nothing may be claimed mirrored while the write failed"
    );
    assert!(f.sidecar("a.cr3").is_none());
    assert!(no_temp_files(&f));
}

#[test]
fn a_renamed_source_requeues_the_intent_and_retries_now() {
    let f = Fix::new("outbox-requeue");
    let a = f.insert_image(7, "a.cr3");
    let revision = save(&f, a);
    let clock = FakeClock::new();

    // The file is renamed between the fenced write succeeding and the outcome being reported:
    // `write_one` returns `Written` (its in-fence re-read already passed), and `complete_outbox`
    // finds a present row at a new path. The intent must survive re-pointed at that identity —
    // deleting it would strand the edit, since relink never re-queues — and the pass must owe
    // an immediate retry, not a backoff.
    let mid_write_db = f.db.clone();
    let rename_row = || {
        let db = mid_write_db.lock().unwrap();
        let old: String = db
            .conn
            .query_row("SELECT path FROM images WHERE id = ?1", [a], |r| r.get(0))
            .unwrap();
        db.conn
            .execute(
                "UPDATE images SET path = ?2, original_filename = 'b.cr3' WHERE id = ?1",
                core_db::rusqlite::params![a, old.replace("a.cr3", "b.cr3")],
            )
            .unwrap();
    };
    let report = drain_with(
        &f.gate,
        &f.db,
        &production_write,
        &clock,
        &never,
        &rename_row,
    )
    .unwrap();
    assert_eq!(report.attempted, 1);
    assert_eq!(report.mirrored, 0, "a moved file's write settles nothing");
    assert_eq!(report.kept, 1, "{report:?}");
    assert_eq!(
        report.next_due,
        Some(0),
        "a requeued intent retries now, not after a backoff"
    );
    let rows = owed(&f);
    assert_eq!(rows.len(), 1, "the intent is re-pointed, not deleted");
    assert!(
        rows[0].path.ends_with("b.cr3"),
        "intent follows the row: {}",
        rows[0].path
    );
    assert_eq!(rows[0].edit_revision, revision);
    assert_eq!(
        rows[0].attempts, 0,
        "old-path failures do not age the new file"
    );
    assert!(f.sidecar("b.cr3").is_none());

    // The immediate retry re-snapshots under a fresh fence and settles at the new identity.
    let report = drain(&f, &production_write, &clock);
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert!(owed(&f).is_empty());
    assert!(
        f.sidecar("b.cr3").is_some(),
        "the sidecar lands beside the moved file"
    );
    assert_eq!(
        mirrored_revision(&f.db.lock().unwrap().conn, a).unwrap(),
        Some((revision, revision))
    );
}

#[test]
fn deleting_the_image_drops_the_intent_without_writing_anything() {
    let f = Fix::new("outbox-deleted");
    let a = f.insert_image(4, "a.cr3");
    save(&f, a);
    f.db.lock()
        .unwrap()
        .conn
        .execute("DELETE FROM images WHERE id = ?1", [a])
        .unwrap();

    let report = drain(&f, &production_write, &FakeClock::new());
    assert_eq!(
        report.attempted, 0,
        "the FK cascade took the intent with the row"
    );
    assert!(owed(&f).is_empty());
    assert!(
        f.sidecar("a.cr3").is_none(),
        "a deleted source never gets a sidecar"
    );
}

#[test]
fn unavailable_sources_do_not_starve_a_healthy_row() {
    let f = Fix::new("outbox-unavailable");
    let clock = FakeClock::new();
    // More missing rows than one page, all older than the healthy one.
    let missing: Vec<i64> = (0..PAGE as u8)
        .map(|i| f.insert_image(i, &format!("m{i}.cr3")))
        .collect();
    for &id in &missing {
        save(&f, id);
        set_status(&f, id, "missing");
    }
    let healthy = f.insert_image(200, "healthy.cr3");
    save(&f, healthy);

    let report = drain(&f, &production_write, &clock);
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert!(f.sidecar("healthy.cr3").is_some(), "healthy row written");
    let rows = owed(&f);
    assert_eq!(
        rows.len(),
        PAGE,
        "missing-source intents are kept, never deleted"
    );
    assert!(rows
        .iter()
        .all(|r| r.next_attempt_at > clock.now_ms() && r.attempts == 0));

    // Nothing is due until the cap passes: a second wake-up attempts nothing.
    let again = drain(&f, &production_write, &clock);
    assert_eq!(again.attempted, 0, "{again:?}");
    assert!(no_temp_files(&f));
}

#[test]
fn permission_failures_do_not_starve_a_healthy_row() {
    let f = Fix::new("outbox-permission");
    let clock = FakeClock::new();
    let bad: Vec<i64> = (0..PAGE as u8)
        .map(|i| f.insert_image(i, &format!("bad{i}.cr3")))
        .collect();
    for &id in &bad {
        save(&f, id);
    }
    let healthy = f.insert_image(200, "good.cr3");
    save(&f, healthy);

    // The staged temp is `.badN.cr3.json.<pid>-<n>.tmp`.
    let write = |w: &SidecarWrite| {
        if w.path.to_string_lossy().contains("/.bad") {
            denied(w)
        } else {
            production_write(w)
        }
    };
    let report = drain(&f, &write, &clock);
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert!(f.sidecar("good.cr3").is_some());
    let rows = owed(&f);
    assert_eq!(rows.len(), PAGE);
    assert!(rows.iter().all(|r| r.attempts == 1), "one attempt each");
}

#[test]
fn remounting_a_source_writes_the_parked_intent_without_a_new_save() {
    let f = Fix::new("outbox-remount");
    let a = f.insert_image(1, "a.cr3");
    save(&f, a);
    set_status(&f, a, "missing");
    let clock = FakeClock::new();
    let report = drain(&f, &production_write, &clock);
    assert_eq!((report.mirrored, report.kept), (0, 1), "{report:?}");
    assert!(f.sidecar("a.cr3").is_none());

    // The card comes back: status flips to present and the schedule is reset (watcher /
    // reconcile / relink do exactly this). No clock movement, no new edit.
    set_status(&f, a, "present");
    reset_outbox_schedule(&f.db.lock().unwrap().conn, Some(a)).unwrap();
    let report = drain(&f, &production_write, &clock);
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert!(f.sidecar("a.cr3").is_some());
    assert!(owed(&f).is_empty());
}

#[test]
fn stop_between_items_leaves_the_rest_queued() {
    let f = Fix::new("outbox-stop");
    let ids: Vec<i64> = (0..5u8)
        .map(|i| f.insert_image(i, &format!("s{i}.cr3")))
        .collect();
    for &id in &ids {
        save(&f, id);
    }
    let written = AtomicUsize::new(0);
    let write = |w: &SidecarWrite| {
        written.fetch_add(1, Ordering::SeqCst);
        production_write(w)
    };
    let stop = || written.load(Ordering::SeqCst) >= 2;
    let report = drain_once(&f.gate, &f.db, &write, &FakeClock::new(), &stop).unwrap();
    assert!(report.interrupted, "{report:?}");
    assert_eq!(report.mirrored, 2);
    assert_eq!(owed(&f).len(), 3, "unvisited rows stay queued");
    let mirrored = ids
        .iter()
        .filter(|&&id| mirrored_revision(&f.db.lock().unwrap().conn, id).unwrap() == Some((1, 1)))
        .count();
    assert_eq!(mirrored, 2, "only written rows are marked mirrored");
    assert_eq!(
        (0..5u8)
            .filter(|i| f.sidecar(&format!("s{i}.cr3")).is_some())
            .count(),
        2
    );
}

/// The timing half of the suite; shares the helpers above via `use super::*;`.
#[path = "scheduling.rs"]
mod scheduling;
