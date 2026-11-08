use super::*;
struct Stub;
impl Clock for Stub {
    fn now_millis(&self) -> u64 {
        0
    }
}
impl EventSink for Stub {
    fn published(&self, _: &SnapshotUpdate) {}
}
impl RenderBackend for Stub {
    fn available(&self) -> bool {
        false
    }
}
#[test]
fn exhausted_counters_do_not_mutate_state() {
    let mut session = ApplicationSession::new(Arc::new(Stub), Arc::new(Stub), Arc::new(Stub));
    session.next_request = u64::MAX;
    let before = session.snapshot();
    assert_eq!(
        session.dispatch(Command::OpenCatalog("test".into())),
        Err(AppError::Exhausted)
    );
    assert_eq!(session.snapshot(), before);
    session.next_request = 0;
    Arc::make_mut(&mut session.snapshot).catalog_epoch = CatalogEpoch(u64::MAX);
    let before = session.snapshot();
    assert_eq!(
        session.dispatch(Command::OpenCatalog("test".into())),
        Err(AppError::Exhausted)
    );
    assert_eq!(session.snapshot(), before);
    Arc::make_mut(&mut session.snapshot).catalog_epoch = CatalogEpoch(0);
    Arc::make_mut(&mut session.snapshot).catalog_ready = true;
    session.selection_generation = SelectionGeneration(u64::MAX);
    let before = session.snapshot();
    assert_eq!(
        session.dispatch(Command::SelectImage(None)),
        Err(AppError::Exhausted)
    );
    assert_eq!(session.snapshot(), before);
    // A refused selection must not have opened (or half-opened) a document.
    assert!(matches!(session.editor.state(), EditorState::Empty));
}
#[test]
fn selection_reserves_room_for_both_effects() {
    let mut session = ApplicationSession::new(Arc::new(Stub), Arc::new(Stub), Arc::new(Stub));
    Arc::make_mut(&mut session.snapshot).catalog_ready = true;
    // One slot left: the row lookup fits, the edit load would not, so nothing is issued.
    session.next_request = u64::MAX - 1;
    let before = session.snapshot();
    assert_eq!(
        session.dispatch(Command::SelectImage(Some(ImageId(1)))),
        Err(AppError::Exhausted)
    );
    assert_eq!(session.snapshot(), before);
    assert!(matches!(session.editor.state(), EditorState::Empty));
    assert!(session.pending.is_empty());
}

fn editable_session() -> ApplicationSession {
    let mut s = ApplicationSession::new(Arc::new(Stub), Arc::new(Stub), Arc::new(Stub));
    {
        let snap = Arc::make_mut(&mut s.snapshot);
        snap.catalog_ready = true;
        snap.editor.selected = Some(ImageId(1));
    }
    let Ok(EditorEvent::Opened(session)) = s.editor.apply(EditorCommand::Open {
        image: ImageId(1),
        default_process_version: CURRENT_PROCESS_VERSION,
    }) else {
        panic!("open");
    };
    s.editor
        .apply(EditorCommand::Loaded {
            session,
            stored: None,
        })
        .unwrap();
    s.key = Some(SaveKey {
        catalog_epoch: s.snapshot.catalog_epoch,
        image_id: ImageId(1),
        content_hash: "00".into(),
    });
    s.sync_document();
    s
}

#[test]
fn edit_is_refused_before_the_document_moves_when_autosave_would_refuse() {
    use crate::workflow::EditIntent;
    let mut s = editable_session();
    s.autosave.stop_admission();
    let before = s.snapshot.editor.document.clone();
    assert!(matches!(
        s.dispatch(Command::Edit(EditIntent::SetExposure(1.0))),
        Err(AppError::EditRefused(_))
    ));
    assert_eq!(s.snapshot.editor.document, before);
    assert!(!s.snapshot.editor.document.dirty);
}

#[test]
fn reset_refuses_while_the_editor_holds_unsaved_work_and_changes_nothing() {
    let mut s = editable_session();
    let session = s.editor.current_session().unwrap();
    // An edit that never reached autosave (cannot happen via commands; forced here).
    s.editor
        .apply(EditorCommand::Apply {
            session,
            params: core_model::DevelopParams {
                exposure: 1.0,
                ..Default::default()
            },
            at_ms: 0,
        })
        .unwrap();
    s.sync_document();
    let epoch = s.snapshot.catalog_epoch;
    assert!(matches!(
        s.dispatch(Command::CloseCatalog),
        Err(AppError::SaveFailed { .. })
    ));
    assert!(s.snapshot.catalog_ready);
    assert_eq!(s.snapshot.catalog_epoch, epoch);
    assert!(matches!(s.editor.state(), EditorState::Ready(_)));
}

#[test]
fn acked_save_from_an_older_catalog_epoch_does_not_patch_rows() {
    use crate::workflow::{Completion, EditIntent};
    use core_model::catalog::ImageRow;
    let mut s = editable_session();
    let row = ImageRow {
        id: 1,
        edited_at: Some(5),
        ..Default::default()
    };
    Arc::make_mut(&mut s.snapshot).library.rows = Arc::new(vec![row]);
    s.dispatch(Command::Edit(EditIntent::SetExposure(1.0)))
        .unwrap();
    let save = s
        .tick(u64::MAX / 2)
        .unwrap()
        .into_iter()
        .find(|e| matches!(e.operation, Operation::SaveEdit { .. }))
        .expect("a save is due");
    // The catalog was replaced while the save was in flight.
    Arc::make_mut(&mut s.snapshot).catalog_epoch = CatalogEpoch(7);
    let result = Ok(Outcome::Saved {
        catalog_revision: CatalogRevision(1),
        mirror: MirrorStatus::Unknown,
        edited_at: 99,
    });
    let _ = s.complete(Completion {
        token: save.token,
        result,
    });
    assert_eq!(s.snapshot.library.rows[0].edited_at, Some(5));
}
