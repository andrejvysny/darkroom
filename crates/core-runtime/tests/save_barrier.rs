//! The save barrier against the real ordered job executor and a real catalog: the barrier must
//! not occupy the ordered `UserCommand` lane, because the flush it asks for is an ordered job.

use core_app::jobs::{JobClass, JobOwner, JobSpec, QueuePolicy};
use core_db::rusqlite::params;
use core_db::Db;
use core_model::identity::CatalogEpoch;
use core_runtime::jobs::{Config, JobHandle, Supervisor};
use core_runtime::save_barrier::{
    verify_revisions, AckVerdict, BarrierId, BarrierReason, BarrierResult, Begin, SaveBarrier,
    SavedRevision, Ticket,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);
const WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: PathBuf,
    db: Arc<Mutex<Db>>,
    image_id: i64,
    sup: Arc<Supervisor>,
    barrier: Arc<SaveBarrier>,
}

impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dr_barrier_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("catalog.db")).unwrap();
        db.conn
            .execute(
                "INSERT INTO images(content_hash, file_size, path, original_filename, status,
                                    capture_date, imported_at)
                 VALUES (?1, 64, '/lib/shot.cr3', 'shot.cr3', 'present', 1000, 0)",
                params![vec![0xabu8; 32]],
            )
            .unwrap();
        let image_id = db.conn.last_insert_rowid();
        let sup = Supervisor::new(
            Config {
                max_jobs: 8,
                max_bytes: 1 << 20,
                max_terminal_records: 16,
                workers: 3,
            },
            Arc::new(|| {}),
        )
        .unwrap();
        Self {
            dir,
            db: Arc::new(Mutex::new(db)),
            image_id,
            sup: Arc::new(sup),
            barrier: Arc::new(SaveBarrier::new()),
        }
    }

    fn spec() -> JobSpec {
        JobSpec {
            owner: JobOwner {
                catalog_epoch: CatalogEpoch(1),
                editor_session: None,
            },
            class: JobClass::UserCommand,
            policy: QueuePolicy::Ordered,
            estimated_bytes: 64,
            durable: true,
        }
    }

    /// The frontend's `develop_set_edit`, as the host submits it: an ordered user-command job.
    fn submit_save(&self, image_id: i64) -> JobHandle<Result<i64, String>> {
        let db = self.db.clone();
        self.sup
            .submit_fallible(Self::spec(), move |_| {
                let db = db.lock().unwrap();
                Ok(core_library::save_edit_with_intent(
                    &db.conn,
                    image_id,
                    5,
                    r#"{"exposure":1}"#,
                    1_700_000_000,
                )
                .map_err(|e| e.to_string()))
            })
            .unwrap()
    }

    fn verify(&self) -> impl Fn(&[SavedRevision]) -> Result<(), String> {
        let db = self.db.clone();
        move |saved| verify_revisions(&db.lock().unwrap().conn, saved)
    }

    fn start(&self, reason: BarrierReason) -> Ticket {
        match self.barrier.begin(reason) {
            Begin::Start(t) => t,
            _ => panic!("barrier must start"),
        }
    }

    /// Run a barrier whose flush request does what the frontend does: submit a save, then ack.
    /// `claim_extra` inflates the claimed revision. Also submits a second ordered job to prove the
    /// lane is free while the barrier waits; returns that job's completion flag.
    fn run_with_real_save(
        &self,
        reason: BarrierReason,
        image_id: i64,
        claim_extra: i64,
    ) -> (BarrierResult, Arc<AtomicBool>) {
        let ticket = self.start(reason);
        let probe_done = Arc::new(AtomicBool::new(false));
        let flag = probe_done.clone();
        let barrier = self.barrier.clone();
        let result = self.barrier.run(
            ticket,
            reason,
            |id: BarrierId| {
                let save = self.submit_save(image_id);
                let probe = self.sup.submit(Self::spec(), |_| Ok(())).unwrap();
                std::thread::spawn(move || {
                    let verdict = match save.wait() {
                        Ok(Ok(revision)) => AckVerdict::ok(vec![SavedRevision {
                            image_id,
                            revision: revision + claim_extra,
                        }]),
                        _ => AckVerdict::failed(),
                    };
                    probe.wait().unwrap();
                    flag.store(true, Ordering::SeqCst);
                    barrier.ack(id, verdict);
                });
                Ok(())
            },
            self.verify(),
            WAIT,
        );
        (result, probe_done)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.sup.stop();
        self.sup.join();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_real_ordered_save_requested_by_the_barrier_completes_it() {
    let f = Fixture::new();
    let (result, probe_done) = f.run_with_real_save(BarrierReason::Quit, f.image_id, 0);
    assert_eq!(result, BarrierResult::Flushed);
    // The ordered lane served the save and a second ordered job while the barrier was waiting.
    assert!(probe_done.load(Ordering::SeqCst));
    let revision: i64 =
        f.db.lock()
            .unwrap()
            .conn
            .query_row(
                "SELECT revision FROM edits WHERE image_id = ?1",
                [f.image_id],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(revision, 1);
}

#[test]
fn a_failed_save_fails_the_barrier_and_retry_follows_the_grace_rules() {
    let f = Fixture::new();
    let (result, _) = f.run_with_real_save(BarrierReason::Quit, 9_999, 0);
    assert!(matches!(result, BarrierResult::Failed(_)), "{result:?}");
    assert!(!result.is_safe());
    // Retry inside the grace: the user chose to discard.
    assert!(matches!(
        f.barrier.begin(BarrierReason::Quit),
        Begin::Proceed
    ));
    // Grace is spent; the next attempt gets the full barrier and, with a working save, flushes.
    let (result, _) = f.run_with_real_save(BarrierReason::Quit, f.image_id, 0);
    assert_eq!(result, BarrierResult::Flushed);
    assert!(matches!(
        f.barrier.begin(BarrierReason::Quit),
        Begin::Start(_)
    ));
}

#[test]
fn a_late_ack_after_timeout_is_ignored_by_the_next_barrier() {
    let f = Fixture::new();
    let ticket = f.start(BarrierReason::Quit);
    let old = ticket.id;
    let result = f.barrier.run(
        ticket,
        BarrierReason::Quit,
        |_| Ok(()),
        f.verify(),
        Duration::from_millis(40),
    );
    assert_eq!(result, BarrierResult::NoAcknowledgement);
    assert!(!result.is_safe());

    // A different reason, so the grace does not short-circuit it.
    let ticket = f.start(BarrierReason::UpdaterRestart);
    assert_ne!(ticket.id, old);
    let barrier = f.barrier.clone();
    let result = f.barrier.run(
        ticket,
        BarrierReason::UpdaterRestart,
        |new| {
            // The stale ack claims failure; if it were accepted the barrier would fail.
            assert!(!barrier.ack(old, AckVerdict::failed()));
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                barrier.ack(new, AckVerdict::ok(vec![]));
            });
            Ok(())
        },
        f.verify(),
        WAIT,
    );
    assert_eq!(result, BarrierResult::Flushed);
}

#[test]
fn a_claim_above_the_committed_revision_fails() {
    let f = Fixture::new();
    let (result, _) = f.run_with_real_save(BarrierReason::Quit, f.image_id, 5);
    match result {
        BarrierResult::Failed(message) => {
            assert!(
                message.contains(&format!("image {}", f.image_id)),
                "{message}"
            )
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn cancel_wakes_the_waiter_as_failed_and_a_later_barrier_works() {
    let f = Fixture::new();
    let ticket = f.start(BarrierReason::Quit);
    let barrier = f.barrier.clone();
    let result = f.barrier.run(
        ticket,
        BarrierReason::Quit,
        |id| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                assert!(barrier.cancel(id));
            });
            Ok(())
        },
        f.verify(),
        WAIT,
    );
    assert_eq!(result, BarrierResult::Failed("cancelled".to_string()));

    let (result, _) = f.run_with_real_save(BarrierReason::UpdaterRestart, f.image_id, 0);
    assert_eq!(result, BarrierResult::Flushed);
}
