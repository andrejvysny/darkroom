//! Flush-barrier semantics of the session: sticky shutdown, admission, timeout, changed sources.

use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::session::{ApplicationSession, BARRIER_TIMEOUT_MS};
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{Command, Completion, EditIntent, Effect, Operation, Outcome};
use core_app::AppError;
use core_model::catalog::ImageRow;
use core_model::document::EditDocument;
use core_model::identity::ImageId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct Clk(AtomicU64);
impl Clock for Clk {
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

fn done(s: &mut ApplicationSession, effect: &Effect, result: Result<Outcome, AppError>) {
    s.complete(Completion {
        token: effect.token,
        result,
    })
    .unwrap();
}

fn row() -> ImageRow {
    ImageRow {
        id: 1,
        content_hash: "00".repeat(32),
        filename: "raw.cr3".into(),
        ..Default::default()
    }
}

/// A session with image 1 open and Ready (no stored edit).
fn ready() -> (ApplicationSession, Arc<Clk>) {
    let clock = Arc::new(Clk::default());
    let mut s = ApplicationSession::new(clock.clone(), Arc::new(Quiet), Arc::new(Quiet));
    let open = s
        .dispatch(Command::OpenCatalog("c".into()))
        .unwrap()
        .remove(0);
    s.complete(Completion {
        token: open.token,
        result: Ok(Outcome::Opened),
    })
    .unwrap();
    let effects = s.dispatch(Command::SelectImage(Some(ImageId(1)))).unwrap();
    done(
        &mut s,
        &effects[0],
        Ok(Outcome::Image(Some(Box::new(row())))),
    );
    done(&mut s, &effects[1], Ok(Outcome::Edit(None)));
    (s, clock)
}

fn expose(s: &mut ApplicationSession, ev: f32) -> Result<(), AppError> {
    s.dispatch(Command::Edit(EditIntent::SetExposure(ev)))
        .map(|_| ())
}

fn saves(effects: &[Effect]) -> Vec<&Effect> {
    effects
        .iter()
        .filter(|e| matches!(e.operation, Operation::SaveEdit { .. }))
        .collect()
}

fn fail(s: &mut ApplicationSession, effect: &Effect, error: AppError) {
    done(s, effect, Err(error));
}

#[test]
fn shutdown_is_sticky_and_a_cancelled_barrier_leaves_admission_open() {
    let (mut s, clock) = ready();
    expose(&mut s, 1.0).unwrap();
    let effects = s.dispatch(Command::RequestShutdown).unwrap();
    assert_eq!(saves(&effects).len(), 1);
    for command in [Command::CloseCatalog, Command::OpenCatalog("other".into())] {
        assert_eq!(
            s.dispatch(command),
            Err(AppError::EditRefused("Darkroom is quitting".into()))
        );
    }
    assert!(s.dispatch(Command::RequestShutdown).unwrap().is_empty());
    fail(&mut s, &effects[0], AppError::Catalog("disk full".into()));
    assert!(s.snapshot().running);

    // A Close barrier that ends without shutting down must not leave admission closed.
    expose(&mut s, 2.0).unwrap();
    let effects = s.dispatch(Command::CloseCatalog).unwrap();
    fail(&mut s, &effects[0], AppError::Catalog("disk full".into()));
    expose(&mut s, 3.0).unwrap();
    clock.0.store(60_000, Ordering::SeqCst);
    let due = s.tick(60_000).unwrap();
    let Operation::SaveEdit { params, .. } = &saves(&due)[0].operation else {
        unreachable!()
    };
    assert_eq!(
        params.exposure, 3.0,
        "the newest edit is recorded and saves"
    );
}

#[test]
fn barrier_times_out_with_save_failed_and_the_late_save_still_completes() {
    let (mut s, clock) = ready();
    expose(&mut s, 1.0).unwrap();
    let effects = s.dispatch(Command::RequestShutdown).unwrap();
    assert_eq!(s.next_deadline(), Some(BARRIER_TIMEOUT_MS));
    clock.0.store(BARRIER_TIMEOUT_MS - 1, Ordering::SeqCst);
    assert!(s.tick(BARRIER_TIMEOUT_MS - 1).unwrap().is_empty());
    assert!(s.snapshot().error.is_none());

    clock.0.store(BARRIER_TIMEOUT_MS, Ordering::SeqCst);
    s.tick(BARRIER_TIMEOUT_MS).unwrap();
    let snap = s.snapshot();
    assert!(snap.running, "a timeout never counts as flushed");
    assert!(matches!(
        &snap.error,
        Some(AppError::SaveFailed { message, .. })
            if message == "Saving did not finish in time; Darkroom stayed open."
    ));
    // The save that was in flight still lands normally afterwards.
    done(
        &mut s,
        &effects[0],
        Ok(Outcome::Saved {
            catalog_revision: core_model::identity::CatalogRevision(1),
            mirror: core_app::state::MirrorStatus::Unknown,
        }),
    );
    assert!(!s.snapshot().editor.document.dirty);
    assert_eq!(
        s.snapshot().editor.save.saved_catalog_revision,
        Some(core_model::identity::CatalogRevision(1))
    );
}

#[test]
fn a_changed_source_blocks_retries_cancels_quit_and_is_cleared_by_an_explicit_discard() {
    let (mut s, clock) = ready();
    expose(&mut s, 1.0).unwrap();
    clock.0.store(300, Ordering::SeqCst);
    let first = s.tick(300).unwrap();
    fail(&mut s, saves(&first)[0], AppError::SourceChanged);

    clock.0.store(100_000, Ordering::SeqCst);
    assert!(
        s.tick(100_000).unwrap().is_empty(),
        "no retry after backoff"
    );
    assert_eq!(s.next_deadline(), None);
    assert!(s.snapshot().editor.document.dirty);

    let refused = s.dispatch(Command::RequestShutdown).unwrap_err();
    let AppError::SaveFailed { image, message } = refused else {
        panic!("expected SaveFailed");
    };
    assert_eq!(image, "raw.cr3");
    assert!(message.contains("changed on disk") && message.contains("raw.cr3"));
    assert!(s.snapshot().running, "quit cancelled");

    // Explicit user decision: drop the edits; the document reloads from storage.
    let effects = s
        .dispatch(Command::DiscardUnsavedEdits(ImageId(1)))
        .unwrap();
    assert_eq!(effects.len(), 2);
    done(
        &mut s,
        &effects[0],
        Ok(Outcome::Image(Some(Box::new(row())))),
    );
    done(
        &mut s,
        &effects[1],
        Ok(Outcome::Edit(Some(EditDocument::read(
            r#"{"exposure":0.5}"#,
            6,
        )))),
    );
    let snap = s.snapshot();
    assert_eq!(snap.editor.document.params.as_ref().unwrap().exposure, 0.5);
    assert!(!snap.editor.document.dirty);

    let close = s.dispatch(Command::RequestShutdown).unwrap();
    assert!(matches!(close[0].operation, Operation::CloseCatalog));
    assert!(!s.snapshot().running);
}

#[test]
fn a_source_change_found_mid_barrier_cancels_it_with_the_file_named() {
    let (mut s, _clock) = ready();
    expose(&mut s, 1.0).unwrap();
    let effects = s.dispatch(Command::CloseCatalog).unwrap();
    fail(&mut s, &effects[0], AppError::SourceChanged);
    let snap = s.snapshot();
    assert!(snap.catalog_ready, "close was cancelled");
    assert!(matches!(
        &snap.error,
        Some(AppError::SaveFailed { image, message })
            if image == "raw.cr3" && message.contains("changed on disk")
    ));
    assert!(matches!(
        s.dispatch(Command::DiscardUnsavedEdits(ImageId(9))),
        Err(AppError::EditRefused(_))
    ));
}

#[test]
fn discard_is_refused_while_the_images_save_is_in_flight() {
    let (mut s, clock) = ready();
    expose(&mut s, 1.0).unwrap();
    clock.0.store(300, Ordering::SeqCst);
    let first = s.tick(300).unwrap();
    assert_eq!(saves(&first).len(), 1);
    assert!(matches!(
        s.dispatch(Command::DiscardUnsavedEdits(ImageId(1))),
        Err(AppError::EditRefused(_))
    ));
    assert!(s.snapshot().editor.document.dirty, "nothing was dropped");
}

#[test]
fn restored_pending_state_uses_the_pending_process_version() {
    use core_app::autosave::IDLE_MS;
    let (mut s, clock) = ready();
    expose(&mut s, 1.0).unwrap();
    // Navigate away and back; the stored document claims a different (older) version.
    assert!(s.dispatch(Command::SelectImage(None)).unwrap().is_empty());
    let effects = s.dispatch(Command::SelectImage(Some(ImageId(1)))).unwrap();
    done(
        &mut s,
        &effects[0],
        Ok(Outcome::Image(Some(Box::new(row())))),
    );
    done(
        &mut s,
        &effects[1],
        Ok(Outcome::Edit(Some(EditDocument::read(
            r#"{"exposure":0.25}"#,
            5,
        )))),
    );
    clock.0.store(IDLE_MS + 1, Ordering::SeqCst);
    let due = s.tick(IDLE_MS + 1).unwrap();
    let Operation::SaveEdit {
        params,
        process_version,
        ..
    } = &saves(&due)[0].operation
    else {
        unreachable!()
    };
    assert_eq!(params.exposure, 1.0, "pending params restored");
    assert_eq!(
        *process_version,
        core_model::identity::CURRENT_PROCESS_VERSION,
        "saves as the pending entry's version, not the stored document's 5"
    );
}
