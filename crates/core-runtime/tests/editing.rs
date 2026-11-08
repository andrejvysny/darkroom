//! Headless end-to-end editing: typed edit commands, autosave on the ordered catalog lane, the
//! flush barrier for close/shutdown, and what the catalog holds afterwards. Real temporary
//! catalogs; only the effect executor is wrapped to inject save delays and failures.

#[path = "../../core-library/tests/support/migration_catalog.rs"]
mod migration_catalog;

use core_app::autosave::IDLE_MS;
use core_app::ports::{CatalogPort, Clock, EffectExecutor, EventSink, RenderBackend};
use core_app::state::{ApplicationSnapshot, DocumentStatus, MirrorStatus};
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{Command, Completion, EditIntent, Effect, Operation};
use core_app::AppError;
use core_db::rusqlite::Connection;
use core_model::identity::{CatalogRevision, ImageId};
use core_runtime::async_workflow::AsyncCatalogWorkflow;
use core_runtime::jobs::Config;
use core_runtime::workflow::{InlineExecutor, RuntimePaths};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now_millis(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Quiet;
impl EventSink for Quiet {
    fn published(&self, _: &SnapshotUpdate) {}
}
impl RenderBackend for Quiet {
    fn available(&self) -> bool {
        false
    }
}

/// Wraps the real executor: signals when a save starts, optionally holds it, optionally fails it.
struct Control {
    fail_saves: AtomicBool,
    gate: Mutex<Option<mpsc::Receiver<()>>>,
    started: Mutex<mpsc::Sender<()>>,
}
struct Exec(Arc<Control>);
impl EffectExecutor for Exec {
    fn execute(&self, effect: Effect, catalog: &dyn CatalogPort) -> Completion {
        if matches!(effect.operation, Operation::SaveEdit { .. }) {
            self.0.started.lock().unwrap().send(()).unwrap();
            let gate = self.0.gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.recv().unwrap();
            }
            if self.0.fail_saves.load(Ordering::SeqCst) {
                return Completion {
                    token: effect.token,
                    result: Err(AppError::Catalog("disk full".into())),
                };
            }
        }
        InlineExecutor.execute(effect, catalog)
    }
}

struct Rig {
    wf: AsyncCatalogWorkflow,
    clock: Arc<TestClock>,
    control: Arc<Control>,
    started: mpsc::Receiver<()>,
    catalog: PathBuf,
}

fn catalog_path() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "darkroom-editing-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    migration_catalog::generate(&dir).unwrap();
    dir.join("copied.db")
}

fn rig_on(catalog: PathBuf) -> Rig {
    let (tx, started) = mpsc::channel();
    let control = Arc::new(Control {
        fail_saves: AtomicBool::new(false),
        gate: Mutex::new(None),
        started: Mutex::new(tx),
    });
    let clock = Arc::new(TestClock::default());
    let mut wf = AsyncCatalogWorkflow::new(
        RuntimePaths {
            catalog: catalog.clone(),
        },
        clock.clone(),
        Arc::new(Quiet),
        Arc::new(Quiet),
        Arc::new(Exec(control.clone())),
        Arc::new(|| {}),
        Config {
            max_jobs: 8,
            max_bytes: 64 * 1024 * 1024,
            max_terminal_records: 16,
            workers: 2,
        },
    )
    .unwrap();
    wf.open_initial().unwrap();
    wf.wait_idle().unwrap();
    Rig {
        wf,
        clock,
        control,
        started,
        catalog,
    }
}

fn rig() -> Rig {
    rig_on(catalog_path())
}

impl Rig {
    fn snap(&self) -> Arc<ApplicationSnapshot> {
        self.wf.snapshot()
    }
    fn select(&mut self, id: i64) {
        self.wf
            .dispatch(Command::SelectImage(Some(ImageId(id))))
            .unwrap();
    }
    fn select_ready(&mut self, id: i64) {
        self.select(id);
        self.wf.wait_idle().unwrap();
        assert_eq!(self.snap().editor.document.status, DocumentStatus::Ready);
    }
    fn edit(&mut self, intent: EditIntent) -> Result<(), AppError> {
        self.wf.dispatch(Command::Edit(intent))
    }
    fn exposure(&self) -> f32 {
        self.snap()
            .editor
            .document
            .params
            .as_ref()
            .unwrap()
            .exposure
    }
    /// Move the clock past the debounce, issue the due save and run the lane to idle.
    fn settle(&mut self) {
        self.clock.0.fetch_add(IDLE_MS + 1, Ordering::SeqCst);
        self.wf.pump().unwrap();
        self.wf.wait_idle().unwrap();
    }
    fn wait_save_started(&self) {
        self.started
            .recv_timeout(Duration::from_secs(5))
            .expect("save did not start");
    }
    fn stored(&self, image: i64) -> (Option<String>, Option<(i64, i64)>) {
        let conn = Connection::open(&self.catalog).unwrap();
        (
            core_library::get_edit(&conn, image).unwrap(),
            core_library::mirrored_revision(&conn, image).unwrap(),
        )
    }
    fn hold_next_save(&self) -> mpsc::Sender<()> {
        let (release, gate) = mpsc::channel();
        *self.control.gate.lock().unwrap() = Some(gate);
        release
    }
}

fn json_of(params: &core_model::DevelopParams) -> String {
    serde_json::to_string(params).unwrap()
}

#[test]
fn open_edit_undo_redo_switch_return_save_shutdown_reopen() {
    let mut rig = rig();
    rig.select_ready(3); // no stored edit: default document
    assert!(!rig.snap().editor.document.dirty);

    rig.edit(EditIntent::SetExposure(1.0)).unwrap();
    assert_eq!(rig.exposure(), 1.0);
    assert!(rig.snap().editor.document.dirty);
    assert!(rig.snap().editor.save.dirty);

    rig.edit(EditIntent::BeginGesture).unwrap();
    let mut dragged = (**rig.snap().editor.document.params.as_ref().unwrap()).clone();
    dragged.exposure = 1.5;
    rig.edit(EditIntent::UpdateGesture(Box::new(dragged)))
        .unwrap();
    rig.edit(EditIntent::CommitGesture).unwrap();
    assert_eq!(rig.exposure(), 1.5);

    rig.edit(EditIntent::Undo).unwrap();
    assert_eq!(rig.exposure(), 1.0, "undo returns to the pre-gesture value");
    assert!(rig.snap().editor.document.can_redo);
    rig.edit(EditIntent::Redo).unwrap();
    assert_eq!(rig.exposure(), 1.5);

    // Switch away and back before the debounce fires: the pending edit comes back.
    rig.select_ready(2);
    rig.select_ready(3);
    assert_eq!(rig.exposure(), 1.5, "pending edit restored");
    assert!(rig.snap().editor.document.dirty);

    rig.settle();
    let snap = rig.snap();
    let acknowledged = snap.editor.document.params.clone().unwrap();
    assert!(!snap.editor.document.dirty, "ack reached the live session");
    assert!(!snap.editor.save.saving);
    let revision = snap.editor.save.saved_catalog_revision.expect("saved");
    assert!(matches!(
        snap.editor.save.mirror,
        MirrorStatus::Pending { owed_revision } if owed_revision == revision
    ));

    rig.wf.dispatch(Command::RequestShutdown).unwrap();
    rig.wf.wait_idle().unwrap();
    assert!(rig.wf.join_shutdown());
    assert!(rig.wf.is_shutdown_complete());
    let catalog = rig.catalog.clone();
    drop(rig);

    let mut again = rig_on(catalog);
    again.select_ready(3);
    assert_eq!(
        **again.snap().editor.document.params.as_ref().unwrap(),
        *acknowledged
    );
    let (json, counters) = again.stored(3);
    assert_eq!(json.unwrap(), json_of(&acknowledged));
    let (durable, mirrored) = counters.unwrap();
    assert_eq!(CatalogRevision(durable), revision);
    assert_eq!(
        mirrored, 0,
        "no sidecar worker ran: pending, never Mirrored"
    );
}

#[test]
fn navigating_away_before_the_save_lands_keeps_the_latest_edit() {
    let mut rig = rig();
    rig.select_ready(3);
    rig.edit(EditIntent::SetExposure(2.0)).unwrap();
    let release = rig.hold_next_save();
    rig.clock.0.fetch_add(IDLE_MS + 1, Ordering::SeqCst);
    rig.wf.pump().unwrap();
    rig.wait_save_started(); // seq 1 (exposure 2.0) is executing, held
    assert!(rig.snap().editor.save.saving);

    rig.edit(EditIntent::SetExposure(2.5)).unwrap(); // newer than the in-flight save
    rig.select(2);
    rig.select(3);
    release.send(()).unwrap();
    rig.wf.wait_idle().unwrap();

    let snap = rig.snap();
    assert_eq!(snap.editor.document.status, DocumentStatus::Ready);
    assert_eq!(
        rig.exposure(),
        2.5,
        "pending state wins over the older stored 2.0"
    );
    assert!(
        snap.editor.document.dirty,
        "the delayed ack of the old session must not mark the new session saved"
    );

    rig.settle();
    assert!(!rig.snap().editor.document.dirty);
    let (json, _) = rig.stored(3);
    let stored: core_model::DevelopParams = serde_json::from_str(&json.unwrap()).unwrap();
    assert_eq!(
        stored.exposure, 2.5,
        "the eventual save persisted the latest params"
    );
}

#[test]
fn closing_the_catalog_waits_for_the_save_in_flight_and_nothing_leaks_into_the_new_epoch() {
    let mut rig = rig();
    rig.select_ready(3);
    rig.edit(EditIntent::SetExposure(1.25)).unwrap();
    let release = rig.hold_next_save();
    rig.clock.0.fetch_add(IDLE_MS + 1, Ordering::SeqCst);
    rig.wf.pump().unwrap();
    rig.wait_save_started();
    let old_epoch = rig.snap().catalog_epoch;

    rig.wf.dispatch(Command::CloseCatalog).unwrap();
    assert!(
        rig.snap().catalog_ready,
        "close is held behind the flush barrier"
    );
    release.send(()).unwrap();
    rig.wf.wait_idle().unwrap();
    assert!(!rig.snap().catalog_ready);
    assert!(rig.snap().catalog_epoch.0 > old_epoch.0);

    let catalog = rig.catalog.clone();
    rig.wf.dispatch(Command::OpenCatalog(catalog)).unwrap();
    rig.wf.wait_idle().unwrap();
    let snap = rig.snap();
    assert!(snap.catalog_epoch.0 > old_epoch.0);
    assert_eq!(
        snap.editor.save,
        Default::default(),
        "no ack in the new epoch"
    );
    assert_eq!(snap.editor.selected, None);
    let (json, _) = rig.stored(3);
    let stored: core_model::DevelopParams = serde_json::from_str(&json.unwrap()).unwrap();
    assert_eq!(
        stored.exposure, 1.25,
        "the barrier flushed the edit before closing"
    );
}

#[test]
fn a_failed_save_cancels_shutdown_and_editing_continues() {
    let mut rig = rig();
    rig.select_ready(3);
    rig.edit(EditIntent::SetExposure(1.0)).unwrap();
    rig.control.fail_saves.store(true, Ordering::SeqCst);

    rig.wf.dispatch(Command::RequestShutdown).unwrap();
    rig.wf.wait_idle().unwrap();
    let snap = rig.snap();
    assert!(snap.running, "shutdown was cancelled");
    assert!(!rig.wf.join_shutdown());
    assert!(!rig.wf.is_shutdown_complete());
    assert!(matches!(
        &snap.error,
        Some(AppError::SaveFailed { message, .. }) if message.contains("disk full")
    ));
    assert!(snap
        .error
        .as_ref()
        .unwrap()
        .to_string()
        .contains("Darkroom stayed open"));
    assert!(snap.editor.document.dirty, "nothing was lost");

    rig.control.fail_saves.store(false, Ordering::SeqCst);
    rig.edit(EditIntent::SetExposure(2.0)).unwrap(); // editing still works
    assert!(rig.snap().error.is_none());

    rig.wf.dispatch(Command::RequestShutdown).unwrap(); // retries the barrier
    rig.wf.wait_idle().unwrap();
    assert!(rig.wf.join_shutdown());
    assert!(rig.wf.is_shutdown_complete());
    let (json, _) = rig.stored(3);
    let stored: core_model::DevelopParams = serde_json::from_str(&json.unwrap()).unwrap();
    assert_eq!(stored.exposure, 2.0);
}

#[test]
fn a_changed_source_is_blocked_cancels_quit_and_only_an_explicit_discard_unblocks_it() {
    let mut rig = rig();
    rig.select_ready(3);
    {
        let conn = Connection::open(&rig.catalog).unwrap();
        conn.execute(
            "UPDATE images SET content_hash = x'0100000000000000000000000000000000000000000000000000000000000000' WHERE id = :id",
            core_db::rusqlite::named_params! { ":id": 3 },
        )
        .unwrap();
    }
    rig.edit(EditIntent::SetExposure(1.0)).unwrap();
    rig.settle();
    rig.wait_save_started();
    let snap = rig.snap();
    assert!(snap.editor.document.dirty);
    let error = snap.editor.save.last_error.clone().expect("visible error");
    assert!(error.contains("changed on disk"), "{error}");
    assert_eq!(snap.editor.save.saved_catalog_revision, None);
    assert_eq!(rig.stored(3).0, None, "nothing was written");

    // Permanent: no retry however long we wait.
    rig.clock.0.fetch_add(120_000, Ordering::SeqCst);
    rig.wf.pump().unwrap();
    rig.wf.wait_idle().unwrap();
    assert!(
        rig.started.try_recv().is_err(),
        "blocked entry is never retried"
    );

    assert!(matches!(
        rig.wf.dispatch(Command::RequestShutdown),
        Err(AppError::SaveFailed { .. })
    ));
    assert!(rig.snap().running);

    rig.wf
        .dispatch(Command::DiscardUnsavedEdits(ImageId(3)))
        .unwrap();
    rig.wf.wait_idle().unwrap();
    assert!(!rig.snap().editor.document.dirty);
    assert_eq!(rig.exposure(), 0.0, "reloaded from storage");

    rig.wf.dispatch(Command::RequestShutdown).unwrap();
    rig.wf.wait_idle().unwrap();
    assert!(rig.wf.join_shutdown());
    assert_eq!(rig.stored(3).0, None, "stored params unchanged");
}

#[test]
fn a_corrupt_stored_edit_refuses_editing_and_writes_nothing() {
    let mut rig = rig();
    rig.select(6);
    rig.wf.wait_idle().unwrap();
    assert_eq!(rig.snap().editor.document.status, DocumentStatus::Corrupt);
    let before = rig.stored(6);
    for intent in [
        EditIntent::SetExposure(1.0),
        EditIntent::BeginGesture,
        EditIntent::Undo,
    ] {
        assert!(matches!(rig.edit(intent), Err(AppError::EditRefused(_))));
    }
    rig.settle();
    assert_eq!(rig.stored(6), before);
    assert!(!rig.snap().editor.document.dirty);
}

#[test]
fn set_cull_persists_through_the_adapter_and_patches_the_snapshot() {
    use core_app::workflow::{ColorLabel, CullChange, CullFlag};
    let mut rig = rig();
    let id = rig.snap().library.rows[0].id;
    for change in [
        CullChange::Rating(5),
        CullChange::Flag(CullFlag::Reject),
        CullChange::Label(Some(ColorLabel::Green)),
    ] {
        rig.wf
            .dispatch(Command::SetCull {
                image: ImageId(id),
                change,
            })
            .unwrap();
    }
    rig.wf.wait_idle().unwrap();
    let conn = Connection::open(&rig.catalog).unwrap();
    let stored: (i64, String, Option<String>) = conn
        .query_row(
            "SELECT stars, flag, color_label FROM ratings_flags WHERE image_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(stored, (5, "reject".into(), Some("green".into())));
    let snap = rig.snap();
    let row = snap.library.rows.iter().find(|r| r.id == id).unwrap();
    assert_eq!((row.stars, row.flag.as_str()), (5, "reject"));
    assert_eq!(row.color_label.as_deref(), Some("green"));
}
