use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::session::ApplicationSession;
use core_app::state::ImageTags;
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{
    Command, Completion, CullChange, CullFlag, Effect, Mutation, MutationReport, Operation, Outcome,
};
use core_app::AppError;
use core_model::catalog::{CollectionRow, ImageRow, KeywordRow, QueryParams, RejectDeleteResult};
use core_model::identity::ImageId;
use std::sync::Arc;

struct ClockStub;
impl Clock for ClockStub {
    fn now_millis(&self) -> u64 {
        42
    }
}
struct Events;
impl EventSink for Events {
    fn published(&self, _: &SnapshotUpdate) {}
}
struct Render;
impl RenderBackend for Render {
    fn available(&self) -> bool {
        true
    }
}
fn session() -> ApplicationSession {
    ApplicationSession::new(Arc::new(ClockStub), Arc::new(Events), Arc::new(Render))
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
/// Opens the catalog and settles every follow-up read with empty data.
fn open_ready() -> ApplicationSession {
    let mut s = session();
    let effect = s
        .dispatch(Command::OpenCatalog("catalog".into()))
        .unwrap()
        .remove(0);
    for effect in finish(&mut s, &effect, Ok(Outcome::Opened)).unwrap() {
        let outcome = match effect.operation {
            Operation::QueryLibrary(_) => Outcome::Library {
                rows: vec![],
                count: 0,
            },
            Operation::LoadFolders => Outcome::Folders(vec![]),
            Operation::LoadDates => Outcome::Dates(vec![]),
            Operation::LoadKeywords => Outcome::Keywords(vec![]),
            Operation::LoadCollections => Outcome::Collections(vec![]),
            Operation::LoadPresets => Outcome::Presets(vec![]),
            other => panic!("unexpected follow-up {other:?}"),
        };
        finish(&mut s, &effect, Ok(outcome)).unwrap();
    }
    s
}
fn ids(values: &[i64]) -> Vec<ImageId> {
    values.iter().map(|v| ImageId(*v)).collect()
}
fn mutate(s: &mut ApplicationSession, mutation: Mutation, report: MutationReport) -> Vec<Effect> {
    let effect = s.dispatch(Command::Mutate(mutation)).unwrap().remove(0);
    assert!(matches!(effect.operation, Operation::Mutate(_)));
    finish(s, &effect, Ok(Outcome::Mutated(report))).unwrap()
}
fn kinds(effects: &[Effect]) -> Vec<&'static str> {
    effects
        .iter()
        .map(|e| match e.operation {
            Operation::QueryLibrary(_) => "query",
            Operation::LoadKeywords => "keywords",
            Operation::LoadCollections => "collections",
            Operation::LoadDates => "dates",
            Operation::LoadFolders => "folders",
            Operation::LoadImageTags(_) => "tags",
            _ => "other",
        })
        .collect()
}
fn has(effects: &[Effect], kind: &str) -> bool {
    kinds(effects).contains(&kind)
}

#[test]
fn validation_refusals() {
    let mut s = open_ready();
    let invalid = |s: &mut ApplicationSession, m: Mutation| {
        assert!(
            matches!(
                s.dispatch(Command::Mutate(m.clone())),
                Err(AppError::InvalidCommand(_))
            ),
            "{m:?}"
        );
    };
    invalid(
        &mut s,
        Mutation::Cull {
            images: vec![],
            change: CullChange::Flag(CullFlag::Pick),
        },
    );
    invalid(
        &mut s,
        Mutation::Cull {
            images: ids(&[1, 0]),
            change: CullChange::Flag(CullFlag::Pick),
        },
    );
    invalid(
        &mut s,
        Mutation::Cull {
            images: ids(&[1]),
            change: CullChange::Rating(6),
        },
    );
    invalid(
        &mut s,
        Mutation::AddKeyword {
            images: ids(&[1]),
            name: "  ".into(),
        },
    );
    invalid(
        &mut s,
        Mutation::AddKeyword {
            images: ids(&[-3]),
            name: "x".into(),
        },
    );
    invalid(&mut s, Mutation::DeleteKeyword(0));
    invalid(
        &mut s,
        Mutation::CreateCollection {
            name: " ".into(),
            smart: None,
            images: vec![],
        },
    );
    invalid(
        &mut s,
        Mutation::CreateCollection {
            name: "s".into(),
            smart: Some(Box::default()),
            images: ids(&[1]),
        },
    );
    invalid(
        &mut s,
        Mutation::RenameCollection {
            id: 1,
            name: "".into(),
        },
    );
    invalid(
        &mut s,
        Mutation::UnlinkPair {
            secondary: ImageId(0),
        },
    );
    assert!(matches!(
        s.dispatch(Command::LoadImageTags(ImageId(0))),
        Err(AppError::InvalidCommand(_))
    ));
}

#[test]
fn mutation_refused_before_open_and_during_shutdown() {
    let mut closed = session();
    assert_eq!(
        closed.dispatch(Command::Mutate(Mutation::DeleteKeyword(1))),
        Err(AppError::NotOpen)
    );
    let mut s = open_ready();
    s.dispatch(Command::RequestShutdown).unwrap();
    // Refused whether the shutdown barrier is pending or the close already ran.
    assert!(matches!(
        s.dispatch(Command::Mutate(Mutation::DeleteKeyword(1))),
        Err(AppError::EditRefused(_) | AppError::NotOpen | AppError::Closed)
    ));
}

#[test]
fn keyword_delete_clears_filter_and_requeries() {
    let mut s = open_ready();
    let effect = s
        .dispatch(Command::QueryLibrary(Box::new(QueryParams {
            keyword_id: Some(7),
            offset: Some(40),
            ..Default::default()
        })))
        .unwrap()
        .remove(0);
    finish(
        &mut s,
        &effect,
        Ok(Outcome::Library {
            rows: vec![],
            count: 0,
        }),
    )
    .unwrap();
    let follow = mutate(&mut s, Mutation::DeleteKeyword(7), MutationReport::Done);
    let query = follow
        .iter()
        .find_map(|e| match &e.operation {
            Operation::QueryLibrary(q) => Some((**q).clone()),
            _ => None,
        })
        .expect("requery");
    assert_eq!(query.keyword_id, None);
    assert_eq!(query.offset, Some(0));
    assert_eq!(s.snapshot().library.query.keyword_id, None);
    assert!(has(&follow, "keywords"));
    assert!(has(&follow, "collections"));
    assert_eq!(s.snapshot().library.tags_revision, 1);
    assert_eq!(s.snapshot().library.last_report, Some(MutationReport::Done));
}

#[test]
fn deleting_the_filtered_collection_resets_the_filter() {
    let mut s = open_ready();
    let effect = s
        .dispatch(Command::QueryLibrary(Box::new(QueryParams {
            collection_id: Some(3),
            ..Default::default()
        })))
        .unwrap()
        .remove(0);
    finish(
        &mut s,
        &effect,
        Ok(Outcome::Library {
            rows: vec![],
            count: 0,
        }),
    )
    .unwrap();
    let follow = mutate(&mut s, Mutation::DeleteCollection(3), MutationReport::Done);
    assert!(has(&follow, "query"));
    assert_eq!(s.snapshot().library.query.collection_id, None);
    // Another collection deleted while filtered by 3 does not touch the grid query.
    let mut s = open_ready();
    let follow = mutate(&mut s, Mutation::DeleteCollection(9), MutationReport::Done);
    assert!(!has(&follow, "query"));
    assert!(has(&follow, "collections"));
}

#[test]
fn cull_patches_rows_in_place() {
    let mut s = open_ready();
    let effect = s
        .dispatch(Command::QueryLibrary(Box::default()))
        .unwrap()
        .remove(0);
    let rows = (1..=3)
        .map(|id| ImageRow {
            id,
            ..Default::default()
        })
        .collect();
    finish(&mut s, &effect, Ok(Outcome::Library { rows, count: 3 })).unwrap();
    let follow = mutate(
        &mut s,
        Mutation::Cull {
            images: ids(&[1, 3]),
            change: CullChange::Rating(4),
        },
        MutationReport::Done,
    );
    assert!(!has(&follow, "query"));
    let snapshot = s.snapshot();
    let stars: Vec<i64> = snapshot.library.rows.iter().map(|r| r.stars).collect();
    assert_eq!(stars, vec![4, 0, 4]);
    // Cull marks are not tags: the info panel must not re-read on every rating.
    assert_eq!(snapshot.library.tags_revision, 0);
    mutate(
        &mut s,
        Mutation::Cull {
            images: ids(&[2]),
            change: CullChange::Flag(CullFlag::Reject),
        },
        MutationReport::Done,
    );
    assert_eq!(s.snapshot().library.rows[1].flag, "reject");
}

#[test]
fn delete_rejected_issues_every_refresh() {
    let mut s = open_ready();
    let report = MutationReport::RejectedDeleted(RejectDeleteResult {
        trashed: 2,
        ..Default::default()
    });
    let follow = mutate(
        &mut s,
        Mutation::DeleteRejected(Box::default()),
        report.clone(),
    );
    for kind in ["query", "keywords", "collections", "dates", "folders"] {
        assert!(has(&follow, kind), "missing {kind}: {:?}", kinds(&follow));
    }
    assert_eq!(s.snapshot().library.last_report, Some(report));
}

#[test]
fn tags_for_another_image_are_rejected() {
    let mut s = open_ready();
    let effect = s
        .dispatch(Command::LoadImageTags(ImageId(5)))
        .unwrap()
        .remove(0);
    let wrong = ImageTags {
        image: ImageId(6),
        keywords: vec![KeywordRow {
            id: 1,
            name: "x".into(),
            count: 0,
        }],
        ..Default::default()
    };
    finish(&mut s, &effect, Ok(Outcome::ImageTags(Box::new(wrong)))).unwrap();
    let snapshot = s.snapshot();
    assert!(snapshot.library.tags.is_none());
    assert!(matches!(snapshot.error, Some(AppError::InvalidCommand(_))));

    let effect = s
        .dispatch(Command::LoadImageTags(ImageId(5)))
        .unwrap()
        .remove(0);
    let right = ImageTags {
        image: ImageId(5),
        collections: vec![CollectionRow {
            id: 2,
            name: "c".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    finish(
        &mut s,
        &effect,
        Ok(Outcome::ImageTags(Box::new(right.clone()))),
    )
    .unwrap();
    assert_eq!(s.snapshot().library.tags.as_deref(), Some(&right));
}

#[test]
fn completion_from_a_replaced_catalog_is_ignored() {
    let mut s = open_ready();
    let effect = s
        .dispatch(Command::Mutate(Mutation::DeleteKeyword(1)))
        .unwrap()
        .remove(0);
    let close = s.dispatch(Command::CloseCatalog).unwrap();
    // The close may have cancelled the mutation effect or be waiting; either way the old
    // completion must not patch state.
    let before = s.snapshot().library.tags_revision;
    let result = finish(&mut s, &effect, Ok(Outcome::Mutated(MutationReport::Done)));
    assert_eq!(result, Err(AppError::Stale));
    assert_eq!(s.snapshot().library.tags_revision, before);
    let _ = close;
}

#[test]
fn mismatched_outcome_is_invalid() {
    let mut s = open_ready();
    let effect = s
        .dispatch(Command::Mutate(Mutation::DeleteKeyword(1)))
        .unwrap()
        .remove(0);
    assert!(matches!(
        finish(&mut s, &effect, Ok(Outcome::Keywords(vec![]))),
        Err(AppError::InvalidCommand(_))
    ));
}
