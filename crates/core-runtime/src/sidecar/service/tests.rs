//! Lifecycle tests for [`MirrorService`]: what is owed reaches disk without the host asking, a
//! service with no catalog shuts down promptly, and a close/reopen re-binds the drain instead of
//! ending the thread. The drain logic itself is [`super::super::outbox`]'s suite; this one only
//! covers the thread, the generation slot and shutdown.
use super::*;
use crate::catalog::CatalogAdapter;
use crate::sidecar::write::tests::Fix;
use core_app::ports::CatalogPort;
use core_db::rusqlite::params;
use std::path::Path;
use std::time::Instant;

const PARAMS: &str = "{\"exposure\":0.5}";
/// Generous: a drain pass is a few catalog reads and one rename, but the thread polls at 1 s.
const DEADLINE: Duration = Duration::from_secs(10);

/// The durable half of a save: the edit and its mirror intent, in one transaction.
fn save(db: &SharedDb, id: i64) {
    let db = db.lock().unwrap();
    core_library::save_edit_with_intent(&db.conn, id, 7, PARAMS, core_library::now_epoch())
        .unwrap();
}

/// A real file on disk plus a `present` row in `db`; `tag` is the content-hash byte. Image id back.
fn insert(db: &SharedDb, dir: &Path, tag: u8, filename: &str) -> i64 {
    let path = dir.join(filename);
    std::fs::write(&path, vec![tag; 64]).unwrap();
    let db = db.lock().unwrap();
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

fn pending(db: &SharedDb) -> i64 {
    let db = db.lock().unwrap();
    db.conn
        .query_row("SELECT COUNT(*) FROM sidecar_outbox", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap()
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let until = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_queued_intent_reaches_disk_without_the_host_asking() {
    let f = Fix::new("service-drain");
    let id = f.insert_image(1, "a.cr3");
    save(&f.db, id);
    let mut service = MirrorService::start(
        Arc::new(CatalogAdapter::from_shared(f.db.clone())),
        Arc::new(|| {}),
    )
    .unwrap();

    wait_until("the queued intent to drain", || pending(&f.db) == 0);
    let sidecar = f
        .sidecar("a.cr3")
        .expect("the sidecar lands beside the RAW");
    assert_eq!(
        sidecar.develop,
        Some(serde_json::from_str::<serde_json::Value>(PARAMS).unwrap()),
        "the file carries the params the catalog committed"
    );
    assert_eq!(sidecar.process_version, Some(7));

    let started = Instant::now();
    service.shutdown().expect("the mirror thread joins");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "shutdown took {:?}",
        started.elapsed()
    );
}

#[test]
fn progress_is_signalled_and_the_catalog_reports_mirrored() {
    use core_app::state::MirrorStatus;
    use std::sync::atomic::AtomicUsize;

    let f = Fix::new("service-progress");
    let id = f.insert_image(4, "p.cr3");
    save(&f.db, id);
    let adapter = Arc::new(CatalogAdapter::from_shared(f.db.clone()));
    let MirrorStatus::Pending { owed_revision } = adapter.mirror_status(id).unwrap() else {
        panic!("a fresh save is owed to the sidecar");
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let mut service = MirrorService::start(
        Arc::clone(&adapter),
        Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }),
    )
    .unwrap();

    wait_until("the progress callback", || calls.load(Ordering::SeqCst) > 0);
    assert!(service.take_progress(), "the flag is raised with the call");
    assert!(!service.take_progress(), "taking it clears it");
    assert_eq!(
        adapter.mirror_status(id).unwrap(),
        MirrorStatus::Mirrored {
            revision: owed_revision
        }
    );
    service.shutdown().expect("the mirror thread joins");
}

#[test]
fn a_service_with_no_open_catalog_shuts_down_promptly() {
    let mut service =
        MirrorService::start(Arc::new(CatalogAdapter::default()), Arc::new(|| {})).unwrap();
    // Nothing to drain and nobody to notify: the thread is parked, so stopping is a wake + join.
    let started = Instant::now();
    service.shutdown().expect("the mirror thread joins");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown took {:?}",
        started.elapsed()
    );
    service.shutdown().expect("shutdown is idempotent");
}

#[test]
fn a_replaced_catalog_rebinds_the_drain() {
    let f = Fix::new("service-reopen");
    let first = f.insert_image(2, "a.cr3");
    save(&f.db, first);
    let adapter = Arc::new(CatalogAdapter::from_shared(f.db.clone()));
    let mut service = MirrorService::start(Arc::clone(&adapter), Arc::new(|| {})).unwrap();
    wait_until("the first catalog to drain", || pending(&f.db) == 0);

    adapter.close().expect("close the catalog");
    // A save committed while nothing is open must not be lost, and must not strand the thread.
    adapter
        .open(&f.dir().join("second.db"))
        .expect("open another");
    let (_generation, db) = adapter.shared().expect("the new catalog is open");
    let second = insert(&db, f.dir(), 3, "b.cr3");
    save(&db, second);

    wait_until("the new generation to drain", || {
        pending(&db) == 0 && f.sidecar("b.cr3").is_some()
    });
    assert!(
        f.sidecar("a.cr3").is_some(),
        "the old catalog already mirrored"
    );
    service
        .shutdown()
        .expect("the thread survived close/reopen and joins");
}
