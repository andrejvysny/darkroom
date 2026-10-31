use core_model::identity::ImageId;
use core_runtime::publication::{PublicationGate, SourceKey};
use std::sync::{Arc, Barrier};

fn key(path: &str) -> SourceKey {
    SourceKey {
        hash: "hash".into(),
        path: path.into(),
    }
}

#[test]
fn staged_publication_cannot_survive_retire_replace_or_reset() {
    for mutation in 0..3 {
        let gate = Arc::new(PublicationGate::default());
        let token = gate.capture(ImageId(1), key("old")).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let worker_gate = Arc::clone(&gate);
        let worker_barrier = Arc::clone(&barrier);
        let worker = std::thread::spawn(move || {
            worker_barrier.wait();
            worker_barrier.wait();
            worker_gate.with_current(&token, || panic!("stale callback ran"))
        });
        barrier.wait();
        match mutation {
            0 => gate.retire(ImageId(1)),
            1 => gate.replace_source(ImageId(1), key("new")),
            _ => gate.invalidate_all(),
        }
        barrier.wait();
        assert!(worker.join().unwrap().is_err());
        if mutation == 0 {
            assert!(gate.capture(ImageId(1), key("old")).is_err());
            gate.register_source(ImageId(1), key("old"));
            assert!(gate.capture(ImageId(1), key("old")).is_ok());
        }
    }
}

#[test]
fn mutation_waits_for_publication_callback() {
    let gate = Arc::new(PublicationGate::default());
    let token = gate.capture(ImageId(1), key("old")).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let worker_gate = Arc::clone(&gate);
    let worker_entered = Arc::clone(&entered);
    let worker_release = Arc::clone(&release);
    let worker = std::thread::spawn(move || {
        worker_gate
            .with_current(&token, || {
                worker_entered.wait();
                worker_release.wait();
            })
            .unwrap();
    });
    entered.wait();
    let mutation_gate = Arc::clone(&gate);
    let mutation = std::thread::spawn(move || mutation_gate.retire(ImageId(1)));
    release.wait();
    worker.join().unwrap();
    mutation.join().unwrap();
    assert!(gate.capture(ImageId(1), key("old")).is_err());
}

#[test]
fn staged_files_and_analysis_are_fenced_by_catalog_mutations() {
    use core_db::rusqlite::Connection;
    use std::sync::{mpsc, Mutex};

    for mutation in 0..4 {
        let gate = Arc::new(PublicationGate::default());
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        db.lock().unwrap().execute_batch(
            "CREATE TABLE images (id INTEGER PRIMARY KEY, path TEXT NOT NULL, hash TEXT NOT NULL); \
             CREATE TABLE analysis (image_id INTEGER NOT NULL); \
             INSERT INTO images VALUES (1, 'old', 'hash');",
        ).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "darkroom-publication-{}-{mutation}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let staged_sidecar = dir.join("sidecar.tmp");
        let staged_thumb = dir.join("thumb.tmp");
        let sidecar = dir.join("sidecar.json");
        let thumb = dir.join("thumb.jpg");
        std::fs::write(&staged_sidecar, b"sidecar").unwrap();
        std::fs::write(&staged_thumb, b"thumb").unwrap();
        let sequence = gate.sequence();
        let token = gate.capture_at(ImageId(1), key("old"), sequence).unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker_gate = Arc::clone(&gate);
        let worker_db = Arc::clone(&db);
        let worker_sidecar = sidecar.clone();
        let worker_thumb = thumb.clone();
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            resume_rx.recv().unwrap();
            worker_gate.with_current(&token, || {
                let current = {
                    let db = worker_db.lock().unwrap();
                    db.query_row(
                        "SELECT path = 'old' AND hash = 'hash' FROM images WHERE id = 1",
                        [],
                        |r| r.get::<_, bool>(0),
                    )
                    .unwrap_or(false)
                };
                assert!(
                    worker_db.try_lock().is_ok(),
                    "DB guard must be released before filesystem publication"
                );
                if current {
                    std::fs::rename(staged_sidecar, worker_sidecar).unwrap();
                    std::fs::rename(staged_thumb, worker_thumb).unwrap();
                    worker_db
                        .lock()
                        .unwrap()
                        .execute("INSERT INTO analysis VALUES (1)", [])
                        .unwrap();
                }
            })
        });
        ready_rx.recv().unwrap();
        match mutation {
            0 => gate.retire_with(ImageId(1), || {
                db.lock()
                    .unwrap()
                    .execute("DELETE FROM images WHERE id = 1", [])
                    .unwrap();
            }),
            1 => gate.replace_source_with(ImageId(1), key("new"), || {
                db.lock()
                    .unwrap()
                    .execute("UPDATE images SET path = 'new' WHERE id = 1", [])
                    .unwrap();
            }),
            2 => gate.invalidate_all_with(|| {
                db.lock()
                    .unwrap()
                    .execute_batch(
                        "DELETE FROM images; INSERT INTO images VALUES (1, 'old', 'hash');",
                    )
                    .unwrap();
            }),
            _ => (),
        }
        resume_tx.send(()).unwrap();
        let result = worker.join().unwrap();
        assert_eq!(result.is_ok(), mutation == 3);
        assert_eq!(sidecar.exists(), mutation == 3);
        assert_eq!(thumb.exists(), mutation == 3);
        let count: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM analysis", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, i64::from(mutation == 3));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn snapshot_sequence_rejects_same_key_delete_and_reimport() {
    let gate = PublicationGate::default();
    let sequence = gate.sequence();
    let old = gate.capture_at(ImageId(1), key("old"), sequence).unwrap();
    gate.retire(ImageId(1));
    gate.register_source(ImageId(1), key("old"));
    assert!(gate.capture_at(ImageId(1), key("old"), sequence).is_err());
    assert!(gate.with_current(&old, || ()).is_err());
    assert!(gate
        .capture_at(ImageId(1), key("old"), gate.sequence())
        .is_ok());
}

#[test]
fn failed_mutation_does_not_revive_captured_token() {
    let gate = PublicationGate::default();
    let token = gate.capture(ImageId(1), key("old")).unwrap();
    let result: Result<(), &str> = gate.retire_with(ImageId(1), || Err("mutation failed"));
    assert!(result.is_err());
    assert!(gate.with_current(&token, || ()).is_err());
    assert!(gate.capture(ImageId(1), key("old")).is_err());
}
