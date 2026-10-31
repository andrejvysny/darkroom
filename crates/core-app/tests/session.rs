use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::session::ApplicationSession;
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{Command, Completion, Effect, Outcome};
use core_app::AppError;
use core_model::catalog::{ImageRow, QueryParams};
use core_model::identity::ImageId;
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
