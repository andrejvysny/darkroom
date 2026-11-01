use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::session::ApplicationSession;
use core_app::state::DocumentStatus;
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{Command, Completion, Effect, Operation, Outcome};
use core_app::AppError;
use core_model::catalog::{ImageRow, QueryParams};
use core_model::document::EditDocument;
use core_model::identity::{ImageId, ProcessVersion, CURRENT_PROCESS_VERSION};
use core_model::DevelopParams;
use std::sync::{Arc, Mutex};

struct ClockStub;
impl Clock for ClockStub {
    fn now_millis(&self) -> u64 {
        42
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<SnapshotUpdate>>);
impl EventSink for Events {
    fn published(&self, update: &SnapshotUpdate) {
        self.0.lock().unwrap().push(update.clone());
    }
}
struct Render;
impl RenderBackend for Render {
    fn available(&self) -> bool {
        true
    }
}
fn session() -> ApplicationSession {
    ApplicationSession::new(
        Arc::new(ClockStub),
        Arc::new(Events::default()),
        Arc::new(Render),
    )
}
fn finish(
    s: &mut ApplicationSession,
    effect: &Effect,
    result: Result<Outcome, AppError>,
) -> Result<Vec<Effect>, AppError> {
    s.complete(Completion {
        token: effect.token,
        result,
    })
}
fn open(s: &mut ApplicationSession) -> Vec<Effect> {
    let effect = s
        .dispatch(Command::OpenCatalog("catalog".into()))
        .unwrap()
        .remove(0);
    finish(s, &effect, Ok(Outcome::Opened)).unwrap()
}
#[test]
fn authorization_payload_and_wrong_outcome() {
    let mut s = session();
    assert_eq!(s.dispatch(Command::RefreshLibrary), Err(AppError::NotOpen));
    let effect = s
        .dispatch(Command::OpenCatalog("catalog".into()))
        .unwrap()
        .remove(0);
    assert!(matches!(
        finish(&mut s, &effect, Ok(Outcome::Closed)),
        Err(AppError::InvalidCommand(_))
    ));
    finish(&mut s, &effect, Ok(Outcome::Opened)).unwrap();
    assert!(matches!(
        s.dispatch(Command::SelectImage(Some(ImageId(0)))),
        Err(AppError::InvalidCommand(_))
    ));
}
#[test]
fn stale_queries_and_aba_selection() {
    let mut s = session();
    let initial = open(&mut s);
    let latest = s
        .dispatch(Command::QueryLibrary(Box::default()))
        .unwrap()
        .remove(0);
    assert_eq!(
        finish(
            &mut s,
            &initial[0],
            Ok(Outcome::Library {
                rows: vec![],
                count: 1
            })
        ),
        Err(AppError::Stale)
    );
    finish(
        &mut s,
        &latest,
        Ok(Outcome::Library {
            rows: vec![],
            count: 2,
        }),
    )
    .unwrap();
    let a = s
        .dispatch(Command::SelectImage(Some(ImageId(1))))
        .unwrap()
        .remove(0);
    s.dispatch(Command::SelectImage(Some(ImageId(2)))).unwrap();
    let again = s
        .dispatch(Command::SelectImage(Some(ImageId(1))))
        .unwrap()
        .remove(0);
    assert_eq!(
        finish(
            &mut s,
            &a,
            Ok(Outcome::Image(Some(Box::new(ImageRow::default()))))
        ),
        Err(AppError::Stale)
    );
    finish(&mut s, &again, Ok(Outcome::Image(None))).unwrap();
    assert_eq!(s.snapshot().library.count, 2);
}
#[test]
fn reset_failure_and_subscriptions() {
    let mut s = session();
    let sub = s.subscribe();
    let first = open(&mut s);
    assert_eq!(
        sub.take_latest().unwrap().snapshot.jobs.active_effect_count,
        3
    );
    let close = s.dispatch(Command::CloseCatalog).unwrap().remove(0);
    assert_eq!(
        finish(
            &mut s,
            &first[0],
            Ok(Outcome::Library {
                rows: vec![],
                count: 0
            })
        ),
        Err(AppError::Stale)
    );
    let reopen = s
        .dispatch(Command::OpenCatalog("other".into()))
        .unwrap()
        .remove(0);
    assert_eq!(
        finish(&mut s, &close, Ok(Outcome::Closed)),
        Err(AppError::Stale)
    );
    let error = AppError::SchemaTooNew {
        found: 5,
        supported: 4,
    };
    finish(&mut s, &reopen, Err(error.clone())).unwrap();
    assert_eq!(s.snapshot().error, Some(error));
    assert!(!s.snapshot().catalog_ready);
    assert_eq!(
        sub.take_latest().unwrap().snapshot.revision,
        s.snapshot().revision
    );
    sub.close();
    s.dispatch(Command::RequestShutdown).unwrap();
    assert!(sub.take_latest().is_none());
}

#[test]
fn identity_notice_validation_and_current_effect() {
    use core_app::registry::ActionId;
    use core_app::subscription::Notice;
    use core_app::workflow::Operation;
    let mut s = session();
    let sub = s.subscribe();
    assert_eq!(sub.take_latest().unwrap().notice, Notice::Initialized);
    let before = s.snapshot();
    assert!(matches!(
        s.dispatch(Command::OpenCatalog("".into())),
        Err(AppError::InvalidCommand(_))
    ));
    assert_eq!(s.snapshot(), before);
    open(&mut s);
    let first = s
        .dispatch(Command::SelectImage(Some(ImageId(1))))
        .unwrap()
        .remove(0);
    assert_eq!(
        sub.take_latest().unwrap().notice,
        Notice::CommandAccepted(ActionId::SelectImage)
    );
    assert!(s.is_current(&first));
    let mut forged = first.clone();
    forged.operation = Operation::LoadDates;
    assert!(!s.is_current(&forged));
    let first_session = s.snapshot().editor.editor_session;
    let second = s
        .dispatch(Command::SelectImage(Some(ImageId(1))))
        .unwrap()
        .remove(0);
    assert_ne!(s.snapshot().editor.editor_session, first_session);
    assert!(!s.is_current(&first));
    let row = ImageRow {
        id: 2,
        ..ImageRow::default()
    };
    assert!(matches!(
        finish(&mut s, &second, Ok(Outcome::Image(Some(Box::new(row))))),
        Err(AppError::InvalidCommand(_))
    ));
    assert!(s.is_current(&second));
    finish(&mut s, &second, Ok(Outcome::Image(None))).unwrap();
    assert_eq!(
        sub.take_latest().unwrap().notice,
        Notice::EffectCompleted {
            action: ActionId::SelectImage,
            result: Ok(())
        }
    );
}

#[test]
fn accepted_command_clears_previous_error() {
    let mut s = session();
    let effect = s
        .dispatch(Command::OpenCatalog("catalog".into()))
        .unwrap()
        .remove(0);
    finish(&mut s, &effect, Err(AppError::Catalog("failed".into()))).unwrap();
    assert!(s.snapshot().error.is_some());
    s.dispatch(Command::OpenCatalog("retry".into())).unwrap();
    assert!(s.snapshot().error.is_none());
}

#[test]
fn failed_changed_query_does_not_relabel_previous_rows() {
    let mut session = session();
    let open = session
        .dispatch(Command::OpenCatalog("catalog".into()))
        .unwrap()
        .remove(0);
    let effects = session
        .complete(Completion {
            token: open.token,
            result: Ok(Outcome::Opened),
        })
        .unwrap();
    let query = effects
        .iter()
        .find(|e| matches!(e.operation, core_app::workflow::Operation::QueryLibrary(_)))
        .unwrap();
    session
        .complete(Completion {
            token: query.token,
            result: Ok(Outcome::Library {
                rows: vec![ImageRow {
                    id: 1,
                    ..Default::default()
                }],
                count: 1,
            }),
        })
        .unwrap();
    let changed = session
        .dispatch(Command::QueryLibrary(Box::new(QueryParams {
            search: Some("other".into()),
            ..Default::default()
        })))
        .unwrap()
        .remove(0);
    assert!(session.snapshot().library.rows.is_empty());
    assert_eq!(session.snapshot().library.count, 0);
    session
        .complete(Completion {
            token: changed.token,
            result: Err(AppError::Catalog("read failed".into())),
        })
        .unwrap();
    assert!(session.snapshot().library.rows.is_empty());
}

// --- selection opens an authoritative editor document (brief WP7-3a) -------------------------

const SUPPORTED_JSON: &str = r#"{"exposure":0.5,"contrast":12}"#;

/// Dispatch a selection and hand back its two effects: the row lookup and the edit load.
fn select(s: &mut ApplicationSession, id: i64) -> (Effect, Effect) {
    let mut effects = s
        .dispatch(Command::SelectImage(Some(ImageId(id))))
        .expect("selection accepted");
    assert_eq!(
        effects.len(),
        2,
        "a selection issues a lookup and an edit load"
    );
    let load = effects.pop().expect("load effect");
    let image = effects.pop().expect("image effect");
    (image, load)
}

fn load_edit_effects(s: &mut ApplicationSession, id: i64) -> (Effect, Effect) {
    let (image, load) = select(s, id);
    assert_eq!(
        image.operation,
        Operation::SelectImage {
            image_id: ImageId(id),
            editor_session: s.snapshot().editor.editor_session,
            selection_generation: s.snapshot().editor.selection_generation,
        }
    );
    assert_eq!(
        load.operation,
        Operation::LoadEdit {
            image_id: ImageId(id),
            editor_session: s.snapshot().editor.editor_session,
        }
    );
    assert!(s.is_current(&image) && s.is_current(&load));
    (image, load)
}

/// Complete a `LoadEdit` and return the resulting document view.
fn finish_load(
    s: &mut ApplicationSession,
    load: &Effect,
    result: Result<Outcome, AppError>,
) -> core_app::state::DocumentView {
    assert!(matches!(load.operation, Operation::LoadEdit { .. }));
    s.complete(Completion {
        token: load.token,
        result,
    })
    .expect("load completion accepted");
    s.snapshot().editor.document.clone()
}

#[test]
fn selection_opens_a_fresh_session_and_asks_for_the_stored_edit() {
    let mut s = session();
    open(&mut s);
    let (image, load) = load_edit_effects(&mut s, 1);
    let snapshot = s.snapshot();
    assert_eq!(snapshot.editor.selected, Some(ImageId(1)));
    assert_eq!(
        snapshot.editor.editor_session.0, 1,
        "editor allocates the session"
    );
    assert_eq!(
        snapshot.editor.document,
        core_app::state::DocumentView {
            status: DocumentStatus::Loading,
            params: None,
            revision: None,
            process_version: None,
            dirty: false,
            can_undo: false,
            can_redo: false,
        }
    );
    // A→B→A still yields a new session every time.
    let second = s
        .dispatch(Command::SelectImage(Some(ImageId(2))))
        .unwrap()
        .remove(0);
    assert_eq!(
        second.operation.action(),
        core_app::registry::ActionId::SelectImage
    );
    assert_eq!(s.snapshot().editor.editor_session.0, 2);
    let again = s
        .dispatch(Command::SelectImage(Some(ImageId(1))))
        .unwrap()
        .remove(0);
    assert_eq!(
        again.operation.action(),
        core_app::registry::ActionId::SelectImage
    );
    assert_eq!(s.snapshot().editor.editor_session.0, 3);
    assert!(!s.is_current(&image));
    assert!(s.is_current(&load), "a superseded load stays completable");
    // Deselecting closes the document without loading anything.
    assert!(s.dispatch(Command::SelectImage(None)).unwrap().is_empty());
    let cleared = s.snapshot();
    assert_eq!(cleared.editor.selected, None);
    assert_eq!(cleared.editor.document.status, DocumentStatus::Empty);
    assert_eq!(cleared.editor.editor_session, Default::default());
}

#[test]
fn supported_edit_loads_with_its_stored_params_and_version() {
    let mut s = session();
    open(&mut s);
    let (_image, load) = load_edit_effects(&mut s, 1);
    let document = EditDocument::read(SUPPORTED_JSON, 5);
    assert_eq!(document.preview().map(|p| p.exposure), Some(0.5));
    let view = finish_load(&mut s, &load, Ok(Outcome::Edit(Some(document.clone()))));
    assert_eq!(view.status, DocumentStatus::Ready);
    assert_eq!(view.params.as_deref(), document.preview());
    assert_eq!(view.process_version, Some(ProcessVersion(5)));
    assert_eq!(view.revision, Some(core_model::identity::EditRevision(1)));
    assert!(!view.dirty, "a freshly loaded stored edit is clean");
    assert!(s.snapshot().error.is_none());
}

#[test]
fn missing_edit_becomes_a_ready_default_document_at_the_current_version() {
    let mut s = session();
    open(&mut s);
    let (_image, load) = load_edit_effects(&mut s, 7);
    let view = finish_load(&mut s, &load, Ok(Outcome::Edit(None)));
    assert_eq!(view.status, DocumentStatus::Ready);
    assert_eq!(view.params.as_deref(), Some(&DevelopParams::default()));
    assert_eq!(view.process_version, Some(CURRENT_PROCESS_VERSION));
    assert!(!view.dirty);
}

#[test]
fn unreadable_stored_edit_is_never_silently_defaulted() {
    // A stored edit this build cannot interpret must surface as a refusal with no params, so no
    // renderer can paint defaults and a later save can never overwrite it.
    let mut s = session();
    open(&mut s);
    let (_image, unsupported) = load_edit_effects(&mut s, 1);
    let view = finish_load(
        &mut s,
        &unsupported,
        Ok(Outcome::Edit(Some(EditDocument::read(SUPPORTED_JSON, 7)))),
    );
    assert!(matches!(view.status, DocumentStatus::Unsupported(_)));
    assert_eq!(view.params, None);
    assert_eq!(view.revision, None);
    assert_eq!(view.process_version, Some(ProcessVersion(7)));
    assert!(!view.dirty);

    let (_image, unknown_fields) = load_edit_effects(&mut s, 2);
    let view = finish_load(
        &mut s,
        &unknown_fields,
        Ok(Outcome::Edit(Some(EditDocument::read(
            r#"{"exposure":0.5,"made_up":1}"#,
            6,
        )))),
    );
    assert!(matches!(view.status, DocumentStatus::Unsupported(_)));
    assert_eq!(view.params, None);

    let (_image, corrupt) = load_edit_effects(&mut s, 3);
    let view = finish_load(
        &mut s,
        &corrupt,
        Ok(Outcome::Edit(Some(EditDocument::read("{not json", 6)))),
    );
    assert_eq!(view.status, DocumentStatus::Corrupt);
    assert_eq!(view.params, None);
    assert_eq!(view.process_version, Some(ProcessVersion(6)));
    assert!(s.snapshot().error.is_none());
}

#[test]
fn load_result_for_a_superseded_session_is_ignored() {
    let mut s = session();
    open(&mut s);
    let (_image_a, load_a) = load_edit_effects(&mut s, 1);
    let (_image_b, load_b) = load_edit_effects(&mut s, 2);
    let session_b = s.snapshot().editor.editor_session;
    // A's stored edit arrives after B was selected: B keeps loading, and none of A's params leak.
    let ignored = finish_load(
        &mut s,
        &load_a,
        Ok(Outcome::Edit(Some(EditDocument::read(SUPPORTED_JSON, 6)))),
    );
    assert_eq!(ignored.status, DocumentStatus::Loading);
    assert_eq!(ignored.params, None);
    assert_eq!(s.snapshot().editor.editor_session, session_b);
    let ready = finish_load(
        &mut s,
        &load_b,
        Ok(Outcome::Edit(Some(EditDocument::read(
            r#"{"contrast":20}"#,
            6,
        )))),
    );
    assert_eq!(ready.status, DocumentStatus::Ready);
    assert_eq!(ready.params.as_deref().map(|p| p.contrast), Some(20.0));
}

#[test]
fn failed_load_fails_the_document_and_leaves_the_application_error_alone() {
    let mut s = session();
    open(&mut s);
    let (_image, load) = load_edit_effects(&mut s, 1);
    // A pre-existing application error must survive: the load failure belongs to the document.
    let query = s
        .dispatch(Command::QueryLibrary(Box::default()))
        .unwrap()
        .remove(0);
    let pre_existing = AppError::Catalog("catalog is busy".into());
    finish(
        &mut s,
        &query,
        Err(AppError::Catalog("catalog is busy".into())),
    )
    .unwrap();
    assert_eq!(s.snapshot().error.as_ref(), Some(&pre_existing));
    let view = finish_load(
        &mut s,
        &load,
        Err(AppError::Catalog("edit read failed".into())),
    );
    assert_eq!(
        view.status,
        DocumentStatus::Error("edit read failed".into())
    );
    assert_eq!(view.params, None);
    assert_eq!(s.snapshot().error, Some(pre_existing));
    assert_eq!(s.snapshot().editor.selected, Some(ImageId(1)));
}

#[test]
fn closing_the_catalog_closes_the_document() {
    let mut s = session();
    open(&mut s);
    let (_image, load) = load_edit_effects(&mut s, 1);
    finish_load(&mut s, &load, Ok(Outcome::Edit(None)));
    assert_eq!(s.snapshot().editor.document.status, DocumentStatus::Ready);
    let close = s.dispatch(Command::CloseCatalog).unwrap().remove(0);
    finish(&mut s, &close, Ok(Outcome::Closed)).unwrap();
    let snapshot = s.snapshot();
    assert_eq!(snapshot.editor.document.status, DocumentStatus::Empty);
    assert_eq!(
        snapshot.editor.document,
        core_app::state::DocumentView::default()
    );
    assert_eq!(snapshot.editor.selected, None);
    // A reopening selection still advances the session counter.
    open(&mut s);
    let (_image, _load) = load_edit_effects(&mut s, 1);
    assert_eq!(s.snapshot().editor.editor_session.0, 2);
}

#[test]
fn set_cull_runs_one_effect_and_patches_rows_on_completion() {
    use core_app::workflow::{ColorLabel, CullChange, CullFlag};
    let mut s = session();
    let effects = open(&mut s);
    let query = effects
        .iter()
        .find(|e| matches!(e.operation, Operation::QueryLibrary(_)))
        .unwrap();
    let row = |id: i64| ImageRow {
        id,
        content_hash: "00".repeat(32),
        filename: format!("{id}.cr3"),
        ..Default::default()
    };
    finish(
        &mut s,
        query,
        Ok(Outcome::Library {
            rows: vec![row(1), row(2)],
            count: 2,
        }),
    )
    .unwrap();
    let (image, _load) = select(&mut s, 1);
    finish(&mut s, &image, Ok(Outcome::Image(Some(Box::new(row(1)))))).unwrap();

    for change in [
        CullChange::Rating(4),
        CullChange::Flag(CullFlag::Pick),
        CullChange::Label(Some(ColorLabel::Blue)),
    ] {
        let mut effects = s
            .dispatch(Command::SetCull {
                image: ImageId(1),
                change,
            })
            .unwrap();
        assert_eq!(effects.len(), 1);
        let effect = effects.remove(0);
        assert_eq!(
            effect.operation,
            Operation::SetCull {
                image_id: ImageId(1),
                change
            }
        );
        finish(
            &mut s,
            &effect,
            Ok(Outcome::CullApplied {
                image_id: ImageId(1),
                change,
            }),
        )
        .unwrap();
    }
    let snap = s.snapshot();
    let patched = &snap.library.rows[0];
    assert_eq!(patched.stars, 4);
    assert_eq!(patched.flag, "pick");
    assert_eq!(patched.color_label.as_deref(), Some("blue"));
    assert_eq!(snap.editor.image.as_ref().unwrap().stars, 4);
    assert_eq!(snap.library.rows[1].stars, 0, "other rows untouched");

    // Failure lands in the application error and leaves rows alone.
    let effect = s
        .dispatch(Command::SetCull {
            image: ImageId(2),
            change: CullChange::Rating(1),
        })
        .unwrap()
        .remove(0);
    finish(&mut s, &effect, Err(AppError::Catalog("disk full".into()))).unwrap();
    assert!(s.snapshot().error.is_some());
    assert_eq!(s.snapshot().library.rows[1].stars, 0);
}

#[test]
fn set_cull_rating_above_five_is_refused() {
    use core_app::workflow::CullChange;
    let mut s = session();
    open(&mut s);
    assert!(matches!(
        s.dispatch(Command::SetCull {
            image: ImageId(1),
            change: CullChange::Rating(6),
        }),
        Err(AppError::InvalidCommand(_))
    ));
}
