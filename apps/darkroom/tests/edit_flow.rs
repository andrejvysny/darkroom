//! End-to-end editing acceptance, headless: the native `NativeApplication` (the exact object
//! the window host drives) over a real temp SQLite catalog holding COPIES of the committed CR3
//! fixture, with an injected manual clock so autosave debounce is deterministic. No window, no
//! GPU. Skips when the fixture is absent; `DARKROOM_REQUIRE_FIXTURES=1` makes that a failure.

// The host's application object, included verbatim (this crate has no lib target).
#[path = "../src/native_app.rs"]
#[allow(dead_code)] // the window-only methods are unused here
mod native_app;

use core_app::ports::Clock;
use core_app::state::{ApplicationSnapshot, DocumentStatus, MirrorStatus};
use core_app::workflow::{Command, EditIntent};
use core_app::AppError;
use core_db::{rusqlite, Db};
use core_library::{add_root, insert_image, process_file, ThumbCache};
use core_model::identity::ImageId;
use native_app::NativeApplication;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

fn fixture() -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../library/2026")
        .canonicalize()
        .ok()?;
    let mut found = Vec::new();
    let mut stack = vec![dir];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).ok().into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("cr3"))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found.into_iter().next()
}

struct Scratch(PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("dr_edit_flow_{tag}_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn catalog(&self) -> PathBuf {
        self.0.join("catalog.db")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Catalog with two images: copies of the fixture (A, B) with distinct content hashes.
fn catalog_with_two_images(tag: &str) -> Option<(Scratch, ImageId, ImageId)> {
    let required = std::env::var_os("DARKROOM_REQUIRE_FIXTURES").is_some();
    let Some(fixture) = fixture() else {
        assert!(
            !required,
            "CR3 fixture missing but DARKROOM_REQUIRE_FIXTURES is set"
        );
        eprintln!("CR3 fixture missing — skipping");
        return None;
    };
    let scratch = Scratch::new(tag);
    let library = scratch.0.join("library");
    fs::create_dir_all(&library).unwrap();
    let a = library.join("_A.CR3");
    let b = library.join("_B.CR3");
    fs::copy(&fixture, &a).unwrap();
    fs::copy(&fixture, &b).unwrap();
    let db = Db::open(&scratch.catalog()).unwrap();
    let thumbs = ThumbCache::new(scratch.0.join("thumbs")).unwrap();
    let folder = add_root(&db.conn, &library).unwrap();
    let mut ids = [0_i64; 2];
    for (slot, path) in ids.iter_mut().zip([&a, &b]) {
        let mut processed = process_file(path, &thumbs, 256).expect("process fixture copy");
        if path == &b {
            // Same bytes, so give B its own identity: the library must hold two real rows.
            processed.content_hash = [0xbb; 32];
            processed.content_hash_hex = "bb".repeat(32);
            processed.capture_fingerprint = None;
        }
        *slot = insert_image(&db.conn, folder, 0, &processed)
            .unwrap()
            .expect("fixture copy indexed");
    }
    Some((scratch, ImageId(ids[0]), ImageId(ids[1])))
}

struct ManualClock(AtomicU64);
impl Clock for ManualClock {
    fn now_millis(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

struct Rig {
    app: NativeApplication,
    clock: Arc<ManualClock>,
}
impl Rig {
    fn open(catalog: &Path) -> Self {
        let clock = Arc::new(ManualClock(AtomicU64::new(1_000_000)));
        let mut app = NativeApplication::with_clock(
            catalog.to_path_buf(),
            false,
            Arc::new(|| {}),
            clock.clone(),
        )
        .expect("open native application");
        app.wait_idle().unwrap();
        assert!(app.snapshot().catalog_ready, "catalog did not open");
        Self { app, clock }
    }
    fn snap(&self) -> Arc<ApplicationSnapshot> {
        self.app.snapshot()
    }
    fn select(&mut self, id: ImageId) {
        self.app.dispatch(Command::SelectImage(Some(id))).unwrap();
        self.app.wait_idle().unwrap();
    }
    fn edit(&mut self, intent: EditIntent) {
        self.app.dispatch(Command::Edit(intent)).unwrap();
    }
    fn exposure(&self) -> f32 {
        self.snap()
            .editor
            .document
            .params
            .as_ref()
            .expect("Ready document has params")
            .exposure
    }
    /// Let the autosave debounce elapse on the manual clock and run the save to completion.
    fn settle(&mut self) {
        assert!(
            self.app.next_deadline().is_some(),
            "a dirty edit must schedule a wake-up"
        );
        self.clock.0.fetch_add(10_000, Ordering::SeqCst);
        self.app.wait_idle().unwrap();
    }
    /// `RequestShutdown`, then drive exactly like the window host's `about_to_wait`.
    fn quit(&mut self) {
        self.app.dispatch(Command::RequestShutdown).unwrap();
        self.app.wait_idle().unwrap();
        self.app.pump().unwrap();
    }
}

fn stored(catalog: &Path, id: ImageId) -> Option<(String, i64)> {
    let conn = rusqlite::Connection::open(catalog).unwrap();
    conn.query_row(
        "SELECT params, revision FROM edits WHERE image_id = ?1",
        [id.0],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

#[test]
fn edit_undo_switch_save_quit_relaunch_restores_exact_exposure() {
    let Some((scratch, a, b)) = catalog_with_two_images("flow") else {
        return;
    };
    let mut rig = Rig::open(&scratch.catalog());
    rig.select(a);
    let snap = rig.snap();
    assert_eq!(snap.editor.document.status, DocumentStatus::Ready);
    assert!(!snap.editor.document.dirty);

    rig.edit(EditIntent::SetExposure(0.7));
    assert_eq!(rig.exposure(), 0.7);
    assert!(rig.snap().editor.save.dirty);

    let mut params = (**rig.snap().editor.document.params.as_ref().unwrap()).clone();
    rig.edit(EditIntent::BeginGesture);
    params.exposure = 1.3;
    rig.edit(EditIntent::UpdateGesture(Box::new(params)));
    rig.edit(EditIntent::CommitGesture);
    assert_eq!(rig.exposure(), 1.3);

    rig.edit(EditIntent::Undo);
    assert_eq!(rig.exposure(), 0.7);
    rig.edit(EditIntent::Redo);
    assert_eq!(rig.exposure(), 1.3);

    // Switch away before anything was saved, and come back: the unsaved edit is restored.
    rig.select(b);
    assert_eq!(rig.exposure(), 0.0);
    rig.select(a);
    assert_eq!(rig.exposure(), 1.3);
    assert!(rig.snap().editor.save.dirty);

    rig.settle();
    let snap = rig.snap();
    let save = &snap.editor.save;
    assert!(!save.dirty && !save.saving, "{save:?}");
    let revision = save
        .saved_catalog_revision
        .expect("catalog save acknowledged");
    // The native host runs no outbox drain: the sidecar mirror is honestly still owed.
    assert_eq!(
        save.mirror,
        MirrorStatus::Pending {
            owed_revision: revision
        }
    );
    let acknowledged = snap.editor.document.params.clone().unwrap();

    rig.quit();
    assert!(rig.app.is_shutdown_complete(), "{:?}", rig.snap().error);
    drop(rig);

    let mut again = Rig::open(&scratch.catalog());
    again.select(a);
    let reloaded = again.snap().editor.document.params.clone().unwrap();
    assert_eq!(reloaded.exposure, 1.3_f32);
    assert_eq!(*reloaded, *acknowledged);
    assert!(!again.snap().editor.save.dirty);
    let (_, db_revision) = stored(&scratch.catalog(), a).expect("edit row");
    assert_eq!(db_revision, revision.0);
}

#[test]
fn failed_save_cancels_quit_keeps_editing_and_discard_unblocks_it() {
    let Some((scratch, a, b)) = catalog_with_two_images("fail") else {
        return;
    };
    let mut rig = Rig::open(&scratch.catalog());
    rig.select(a);
    rig.edit(EditIntent::SetExposure(0.5));
    rig.settle();
    let before = stored(&scratch.catalog(), a).expect("A saved");

    rig.edit(EditIntent::SetExposure(1.0));
    {
        // The source changes underneath the unsaved edit.
        let conn = rusqlite::Connection::open(scratch.catalog()).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let changed = conn
            .execute(
                "UPDATE images SET content_hash = zeroblob(32) WHERE id = ?1",
                [a.0],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    rig.quit();
    assert!(!rig.app.is_shutdown_complete(), "quit must be cancelled");
    let snap = rig.snap();
    assert!(snap.running, "session must stay running");
    match snap.error.as_ref() {
        Some(AppError::SaveFailed { image, .. }) => assert_eq!(image, "_A.CR3"),
        Some(AppError::SourceChanged) => {}
        other => panic!("expected a save error naming A, got {other:?}"),
    }
    assert!(snap.editor.save.dirty, "A's edit must still be pending");

    // Editing continues on another image and its save succeeds.
    rig.select(b);
    rig.edit(EditIntent::SetExposure(0.3));
    rig.settle();
    assert!(!rig.snap().editor.save.dirty);
    assert!(stored(&scratch.catalog(), b).is_some(), "B saved");
    // A retried quit is refused up front while A stays blocked; the window stays open.
    assert!(matches!(
        rig.app.dispatch(Command::RequestShutdown),
        Err(AppError::SaveFailed { .. })
    ));
    assert!(!rig.app.is_shutdown_complete());

    rig.app.dispatch(Command::DiscardUnsavedEdits(a)).unwrap();
    rig.app.wait_idle().unwrap();
    rig.quit();
    assert!(rig.app.is_shutdown_complete(), "{:?}", rig.snap().error);
    drop(rig);
    assert_eq!(stored(&scratch.catalog(), a), Some(before), "A untouched");
}
