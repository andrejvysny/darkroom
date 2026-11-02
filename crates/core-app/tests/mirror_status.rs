//! `RefreshMirrorStatus`: the saved edit's sidecar state advances once the mirror publishes, and
//! only for the exact catalog epoch / image / saved revision that asked.

use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::session::ApplicationSession;
use core_app::state::MirrorStatus;
use core_app::subscription::SnapshotUpdate;
use core_app::workflow::{Command, Completion, EditIntent, Effect, Operation, Outcome};
use core_app::AppError;
use core_model::catalog::ImageRow;
use core_model::identity::{CatalogRevision, ImageId};
use std::sync::Arc;

struct Quiet;
impl Clock for Quiet {
    fn now_millis(&self) -> u64 {
        0
    }
}
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

fn rev(n: i64) -> CatalogRevision {
    CatalogRevision(n)
}

fn mirror(s: &ApplicationSession) -> MirrorStatus {
    s.snapshot().editor.save.mirror.clone()
}

fn open() -> ApplicationSession {
    let mut s = ApplicationSession::new(Arc::new(Quiet), Arc::new(Quiet), Arc::new(Quiet));
    let effect = s
        .dispatch(Command::OpenCatalog("c".into()))
        .unwrap()
        .remove(0);
    done(&mut s, &effect, Ok(Outcome::Opened));
    s
}

fn select(s: &mut ApplicationSession, id: i64) {
    let effects = s.dispatch(Command::SelectImage(Some(ImageId(id)))).unwrap();
    let row = ImageRow {
        id,
        content_hash: "00".repeat(32),
        filename: "raw.cr3".into(),
        ..Default::default()
    };
    done(s, &effects[0], Ok(Outcome::Image(Some(Box::new(row)))));
    done(s, &effects[1], Ok(Outcome::Edit(None)));
}

/// Edit the selected image and acknowledge its save as `revision` with `mirror`.
fn save(s: &mut ApplicationSession, ev: f32, revision: i64, mirror: MirrorStatus) {
    s.dispatch(Command::Edit(EditIntent::SetExposure(ev)))
        .unwrap();
    let due = s.tick(u64::MAX / 2).unwrap();
    let save = due
        .iter()
        .find(|e| matches!(e.operation, Operation::SaveEdit { .. }))
        .expect("a save is due");
    done(
        s,
        save,
        Ok(Outcome::Saved {
            catalog_revision: rev(revision),
            mirror,
            edited_at: 0,
        }),
    );
}

fn refresh(s: &mut ApplicationSession) -> Vec<Effect> {
    s.dispatch(Command::RefreshMirrorStatus).unwrap()
}

fn pending(n: i64) -> MirrorStatus {
    MirrorStatus::Pending {
        owed_revision: rev(n),
    }
}

fn mirrored(n: i64) -> MirrorStatus {
    MirrorStatus::Mirrored { revision: rev(n) }
}

#[test]
fn pending_save_becomes_mirrored_when_the_refresh_completes() {
    let mut s = open();
    select(&mut s, 1);
    save(&mut s, 1.0, 1, pending(1));
    assert_eq!(mirror(&s), pending(1));

    let effects = refresh(&mut s);
    assert_eq!(effects.len(), 1);
    let Operation::LoadMirrorStatus { key, revision } = &effects[0].operation else {
        panic!("expected LoadMirrorStatus, got {:?}", effects[0].operation);
    };
    assert_eq!((key.image_id, *revision), (ImageId(1), rev(1)));
    done(&mut s, &effects[0], Ok(Outcome::MirrorStatus(mirrored(1))));
    assert_eq!(mirror(&s), mirrored(1));
}

#[test]
fn refresh_does_nothing_unless_a_save_is_unsettled() {
    let mut s = open();
    assert!(refresh(&mut s).is_empty(), "nothing selected");
    select(&mut s, 1);
    assert!(refresh(&mut s).is_empty(), "nothing saved");
    save(&mut s, 1.0, 1, MirrorStatus::Unknown);
    assert!(refresh(&mut s).is_empty(), "unknown mirror");
    save(&mut s, 2.0, 2, mirrored(2));
    assert!(refresh(&mut s).is_empty(), "already mirrored");
}

#[test]
fn a_result_for_an_older_saved_revision_is_ignored() {
    let mut s = open();
    select(&mut s, 1);
    save(&mut s, 1.0, 1, pending(1));
    let effects = refresh(&mut s);
    save(&mut s, 2.0, 2, pending(2));
    done(&mut s, &effects[0], Ok(Outcome::MirrorStatus(mirrored(1))));
    assert_eq!(mirror(&s), pending(2));
}

#[test]
fn a_result_for_another_selection_never_reaches_it() {
    let mut s = open();
    select(&mut s, 1);
    save(&mut s, 1.0, 1, pending(1));
    let effects = refresh(&mut s);
    select(&mut s, 2);
    assert_eq!(mirror(&s), MirrorStatus::Unknown);
    let _ = s.complete(Completion {
        token: effects[0].token,
        result: Ok(Outcome::MirrorStatus(mirrored(1))),
    });
    assert_eq!(mirror(&s), MirrorStatus::Unknown);

    select(&mut s, 1);
    let _ = s.complete(Completion {
        token: effects[0].token,
        result: Ok(Outcome::MirrorStatus(mirrored(1))),
    });
    assert_eq!(
        mirror(&s),
        MirrorStatus::Unknown,
        "the reselected image has no saved revision yet"
    );
}

#[test]
fn failing_then_mirrored_is_accepted_and_stale_reads_are_not() {
    let mut s = open();
    select(&mut s, 1);
    save(&mut s, 1.0, 3, pending(3));

    let effects = refresh(&mut s);
    done(&mut s, &effects[0], Ok(Outcome::MirrorStatus(mirrored(2))));
    assert_eq!(mirror(&s), pending(3), "older mirrored revision ignored");

    let failing = MirrorStatus::Failing {
        error: "disk full".into(),
    };
    let effects = refresh(&mut s);
    done(
        &mut s,
        &effects[0],
        Ok(Outcome::MirrorStatus(failing.clone())),
    );
    assert_eq!(mirror(&s), failing);

    let effects = refresh(&mut s);
    assert_eq!(effects.len(), 1, "a failing mirror is still refreshable");
    done(&mut s, &effects[0], Ok(Outcome::MirrorStatus(mirrored(3))));
    assert_eq!(mirror(&s), mirrored(3));
}

#[test]
fn a_failed_read_leaves_the_status_and_error_alone() {
    let mut s = open();
    select(&mut s, 1);
    save(&mut s, 1.0, 1, pending(1));
    let effects = refresh(&mut s);
    done(&mut s, &effects[0], Err(AppError::Catalog("busy".into())));
    assert_eq!(mirror(&s), pending(1));
    assert!(s.snapshot().error.is_none());
}

fn library_row(id: i64, edited_at: Option<i64>) -> ImageRow {
    ImageRow {
        id,
        content_hash: "00".repeat(32),
        filename: format!("{id}.cr3"),
        edited_at,
        ..Default::default()
    }
}

fn row_edited_at(s: &ApplicationSession, id: i64) -> Option<i64> {
    s.snapshot()
        .library
        .rows
        .iter()
        .find(|row| row.id == id)
        .and_then(|row| row.edited_at)
}

/// Load `rows` as the library, select image 1, edit it, and return its due save effect.
fn library_with_due_save(s: &mut ApplicationSession, rows: Vec<ImageRow>, ev: f32) -> Effect {
    let effects = s.dispatch(Command::RefreshLibrary).unwrap();
    let query = effects
        .iter()
        .find(|e| matches!(e.operation, Operation::QueryLibrary(_)))
        .expect("library query");
    let count = rows.len() as i64;
    done(s, query, Ok(Outcome::Library { rows, count }));
    select(s, 1);
    s.dispatch(Command::Edit(EditIntent::SetExposure(ev)))
        .unwrap();
    s.tick(u64::MAX / 2)
        .unwrap()
        .into_iter()
        .find(|e| matches!(e.operation, Operation::SaveEdit { .. }))
        .expect("a save is due")
}

fn saved(edited_at: i64) -> Outcome {
    Outcome::Saved {
        catalog_revision: rev(1),
        mirror: MirrorStatus::Unknown,
        edited_at,
    }
}

#[test]
fn acked_save_patches_library_row_edited_at_and_never_lowers_it() {
    let mut s = open();
    let save = library_with_due_save(
        &mut s,
        vec![library_row(1, Some(50)), library_row(2, None)],
        1.0,
    );
    done(&mut s, &save, Ok(saved(40)));
    assert_eq!(row_edited_at(&s, 1), Some(50), "a lower value never lowers");
    assert_eq!(row_edited_at(&s, 2), None, "other images are untouched");

    let save = library_with_due_save(
        &mut s,
        vec![library_row(1, Some(50)), library_row(2, None)],
        2.0,
    );
    done(&mut s, &save, Ok(saved(90)));
    assert_eq!(row_edited_at(&s, 1), Some(90));
    assert_eq!(
        s.snapshot().editor.image.as_ref().unwrap().edited_at,
        Some(90)
    );
}

#[test]
fn acked_save_of_a_non_selected_image_still_patches_its_row() {
    let mut s = open();
    let save = library_with_due_save(
        &mut s,
        vec![library_row(1, None), library_row(2, None)],
        1.0,
    );
    // The user moves on before the ack lands.
    select(&mut s, 2);
    done(&mut s, &save, Ok(saved(70)));
    assert_eq!(row_edited_at(&s, 1), Some(70));
    assert_eq!(row_edited_at(&s, 2), None);
}
