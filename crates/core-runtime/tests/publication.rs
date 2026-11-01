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

#[test]
fn commit_guard_rejects_after_reset() {
    let gate = PublicationGate::default();
    let epoch = gate.epoch();
    drop(gate.invalidate_all_guard());
    assert!(gate.commit_guard(epoch).is_err());
    assert!(gate.commit_guard(gate.epoch()).is_ok());
}

#[test]
fn commit_guard_tolerates_unrelated_source_mutation() {
    let gate = PublicationGate::default();
    let epoch = gate.epoch();
    gate.replace_source(ImageId(9), key("/x"));
    assert!(gate.commit_guard(epoch).is_ok());
}

#[test]
fn guard_replace_source_invalidates_old_token_and_admits_new_key() {
    let gate = PublicationGate::default();
    let token = gate.capture(ImageId(1), key("/old")).unwrap();
    let epoch = gate.epoch();
    {
        let mut guard = gate.commit_guard(epoch).unwrap();
        guard.replace_source(ImageId(1), key("/new"));
    }
    assert!(gate.with_current(&token, || ()).is_err());
    let replacement = gate.capture(ImageId(1), key("/new")).unwrap();
    assert!(gate.with_current(&replacement, || ()).is_ok());
}

#[test]
fn guard_retire_blocks_all_captures_of_id() {
    let gate = PublicationGate::default();
    let epoch = gate.epoch();
    {
        let mut guard = gate.commit_guard(epoch).unwrap();
        guard.retire(ImageId(2));
    }
    assert!(gate.capture(ImageId(2), key("/a")).is_err());
}

#[test]
fn reused_id_cannot_match_previous_source() {
    let gate = PublicationGate::default();
    let token = gate
        .capture(
            ImageId(3),
            SourceKey {
                hash: "h1".into(),
                path: "/a".into(),
            },
        )
        .unwrap();
    let epoch = gate.epoch();
    {
        let mut guard = gate.commit_guard(epoch).unwrap();
        guard.retire(ImageId(3));
        // Same id, same path, different bytes: SQLite rowid reuse must not resurrect the token.
        guard.replace_source(
            ImageId(3),
            SourceKey {
                hash: "h2".into(),
                path: "/a".into(),
            },
        );
    }
    assert!(gate.with_current(&token, || ()).is_err());
}

#[test]
fn commit_guard_blocks_concurrent_reset_until_dropped() {
    use std::sync::mpsc;
    use std::time::Duration;

    let gate = Arc::new(PublicationGate::default());
    let epoch = gate.epoch();
    let (held_tx, held_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder_gate = Arc::clone(&gate);
    let holder = std::thread::spawn(move || {
        let guard = holder_gate.commit_guard(epoch).unwrap();
        held_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        drop(guard);
    });
    held_rx.recv().unwrap();
    let (reset_tx, reset_rx) = mpsc::channel::<()>();
    let reset_gate = Arc::clone(&gate);
    let reset = std::thread::spawn(move || {
        let guard = reset_gate.invalidate_all_guard();
        reset_tx.send(()).unwrap();
        drop(guard);
    });
    assert!(reset_rx.recv_timeout(Duration::from_millis(50)).is_err());
    release_tx.send(()).unwrap();
    holder.join().unwrap();
    reset_rx.recv().unwrap();
    reset.join().unwrap();
    assert!(gate.commit_guard(epoch).is_err());
    assert!(gate.commit_guard(gate.epoch()).is_ok());
}

#[test]
fn merge_provenance_is_rejected_after_source_delete_and_rowid_reuse() {
    use core_runtime::publication::Stale;
    use std::sync::atomic::{AtomicBool, Ordering};

    let gate = PublicationGate::default();
    let (path_a, path_b) = ("/library/a.cr3", "/library/b.cr3");
    gate.register_source(
        ImageId(1),
        SourceKey {
            hash: "a".into(),
            path: path_a.into(),
        },
    );
    gate.register_source(
        ImageId(2),
        SourceKey {
            hash: "b".into(),
            path: path_b.into(),
        },
    );
    // What `source_fence::capture_sources` does before the merge's minutes of unlocked work.
    let sequence = gate.sequence();
    let tokens = [
        gate.capture_at(
            ImageId(1),
            SourceKey {
                hash: "a".into(),
                path: path_a.into(),
            },
            sequence,
        )
        .unwrap(),
        gate.capture_at(
            ImageId(2),
            SourceKey {
                hash: "b".into(),
                path: path_b.into(),
            },
            sequence,
        )
        .unwrap(),
    ];
    // Cull/dedup delete retires the whole source set, then SQLite reuses rowid 2 for a
    // different photo at the same path.
    drop(gate.invalidate_all_guard());
    gate.replace_source(
        ImageId(2),
        SourceKey {
            hash: "z".into(),
            path: path_b.into(),
        },
    );
    let ran = AtomicBool::new(false);
    assert!(matches!(
        gate.with_tokens(&tokens, || ran.store(true, Ordering::SeqCst)),
        Err(Stale)
    ));
    assert!(
        !ran.load(Ordering::SeqCst),
        "provenance insert ran despite stale tokens"
    );
}

#[test]
fn reused_rowid_is_publishable_after_registration() {
    let gate = PublicationGate::default();
    let key_a = SourceKey {
        hash: "a".into(),
        path: "/library/x.cr3".into(),
    };
    let key_b = SourceKey {
        hash: "b".into(),
        path: "/library/y.cr3".into(),
    };
    let token_a = gate.capture(ImageId(5), key_a.clone()).unwrap();
    // A delete retires the row (trash flow / watcher reconcile).
    gate.source_guard().retire(ImageId(5));
    // SQLite reuses rowid 5 (no AUTOINCREMENT) for different bytes at a new path. Until the
    // insert registers that identity, nothing may publish against the id — this refusal is
    // intentional and must not be relaxed.
    assert!(
        gate.capture(ImageId(5), key_b.clone()).is_err(),
        "a retired id refuses every key until registration"
    );
    // The fenced commit registers the new identity while holding the gate...
    gate.source_guard()
        .replace_source(ImageId(5), key_b.clone());
    // ...so work for the reused rowid is publishable immediately, with no app restart.
    let token_b = gate
        .capture(ImageId(5), key_b.clone())
        .expect("reused rowid is publishable after registration");
    assert!(gate.with_current(&token_b, || ()).is_ok());
    // The deleted row's old token stays dead, and the old key stays unrecoverable.
    assert!(gate.with_current(&token_a, || ()).is_err());
    assert!(gate.capture(ImageId(5), key_a).is_err());
}

#[test]
fn tokens_guard_matches_with_tokens() {
    use core_runtime::publication::Stale;
    let gate = PublicationGate::default();
    gate.register_source(
        ImageId(1),
        SourceKey {
            hash: "a".into(),
            path: "/a".into(),
        },
    );
    gate.register_source(
        ImageId(2),
        SourceKey {
            hash: "b".into(),
            path: "/b".into(),
        },
    );
    let tokens = [
        gate.capture(
            ImageId(1),
            SourceKey {
                hash: "a".into(),
                path: "/a".into(),
            },
        )
        .unwrap(),
        gate.capture(
            ImageId(2),
            SourceKey {
                hash: "b".into(),
                path: "/b".into(),
            },
        )
        .unwrap(),
    ];
    // Valid set: both entry points admit the caller.
    assert!(gate.with_tokens(&tokens, || ()).is_ok());
    assert!(gate.tokens_guard(&tokens).is_ok());
    // Stale set (one source retired): both reject; the guard hands out nothing.
    gate.source_guard().retire(ImageId(2));
    assert!(matches!(
        gate.with_tokens(&tokens, || unreachable!("stale set must not run")),
        Err(Stale)
    ));
    assert!(matches!(gate.tokens_guard(&tokens), Err(Stale)));
    // The guard path is also the HDR/panorama commit shape: register a fresh id under the
    // still-valid remaining token set without releasing the gate.
    let valid = [tokens[0].clone()];
    {
        let mut guard = gate
            .tokens_guard(&valid)
            .expect("remaining source is current");
        guard.replace_source(
            ImageId(2),
            SourceKey {
                hash: "c".into(),
                path: "/c".into(),
            },
        );
    }
    assert!(gate
        .capture(
            ImageId(2),
            SourceKey {
                hash: "c".into(),
                path: "/c".into(),
            }
        )
        .is_ok());
}
