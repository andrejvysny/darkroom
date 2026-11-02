//! Presets, paste, snapshot restore and revert/reset at the session: the resolve fence, one
//! undoable step per apply, and the snapshot list lifecycle.

use core_app::ports::{Clock, EventSink, RenderBackend};
use core_app::session::ApplicationSession;
use core_app::subscription::{Notice, SnapshotUpdate};
use core_app::workflow::{
    Command, Completion, EditIntent, Effect, Mutation, MutationReport, Operation, Outcome,
    SettingsSource,
};
use core_app::AppError;
use core_model::catalog::{ImageRow, PresetSummary, SnapshotSummary};
use core_model::document::{params_to_json, EditDocument};
use core_model::identity::ImageId;
use core_model::DevelopParams;
use std::sync::{Arc, Mutex};

struct Clk;
impl Clock for Clk {
    fn now_millis(&self) -> u64 {
        10
    }
}
#[derive(Default)]
struct Events(Mutex<Vec<Notice>>);
impl EventSink for Events {
    fn published(&self, update: &SnapshotUpdate) {
        self.0.lock().unwrap().push(update.notice.clone());
    }
}
struct Render;
impl RenderBackend for Render {
    fn available(&self) -> bool {
        false
    }
}

fn row(id: i64) -> ImageRow {
    ImageRow {
        id,
        content_hash: format!("{id:02x}").repeat(32),
        filename: format!("{id}.cr3"),
        ..Default::default()
    }
}

fn done(
    s: &mut ApplicationSession,
    effect: &Effect,
    result: Result<Outcome, AppError>,
) -> Vec<Effect> {
    s.complete(Completion {
        token: effect.token,
        result,
    })
    .unwrap()
}

fn exposed(ev: f32) -> DevelopParams {
    DevelopParams {
        exposure: ev,
        ..Default::default()
    }
}

fn exposure(s: &ApplicationSession) -> f32 {
    s.snapshot()
        .editor
        .document
        .params
        .as_ref()
        .unwrap()
        .exposure
}

/// Catalog open and image 1 Ready with `stored` as its opened document.
fn ready(stored: Option<DevelopParams>) -> (ApplicationSession, Arc<Events>) {
    let events = Arc::new(Events::default());
    let mut s = ApplicationSession::new(Arc::new(Clk), events.clone(), Arc::new(Render));
    let open = s
        .dispatch(Command::OpenCatalog("c".into()))
        .unwrap()
        .remove(0);
    done(&mut s, &open, Ok(Outcome::Opened));
    select(&mut s, 1, stored);
    (s, events)
}

/// Select `id`; returns the follow-up effects of its row completion.
fn select(s: &mut ApplicationSession, id: i64, stored: Option<DevelopParams>) -> Vec<Effect> {
    let effects = s.dispatch(Command::SelectImage(Some(ImageId(id)))).unwrap();
    let follow = done(s, &effects[0], Ok(Outcome::Image(Some(Box::new(row(id))))));
    let document = stored.map(|p| EditDocument::read(params_to_json(&p).unwrap(), 6));
    done(s, &effects[1], Ok(Outcome::Edit(document)));
    follow
}

fn apply_copied(s: &mut ApplicationSession, params: DevelopParams) -> Effect {
    let mut effects = s
        .dispatch(Command::ApplySettings {
            source: SettingsSource::Copied {
                params: Arc::new(params),
                field_keys: None,
            },
            amount: 1.0,
            replace_all: false,
        })
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert!(matches!(
        effects[0].operation,
        Operation::ResolveSettings { .. }
    ));
    effects.remove(0)
}

fn resolved(params: DevelopParams) -> Result<Outcome, AppError> {
    Ok(Outcome::SettingsResolved(Box::new(params)))
}

#[test]
fn a_current_resolve_is_one_undoable_edit() {
    let (mut s, _) = ready(None);
    let effect = apply_copied(&mut s, exposed(2.0));
    done(&mut s, &effect, resolved(exposed(2.0)));
    assert_eq!(exposure(&s), 2.0);
    assert!(s.snapshot().editor.document.can_undo);
    s.dispatch(Command::Edit(EditIntent::Undo)).unwrap();
    assert_eq!(exposure(&s), 0.0);
    assert!(!s.snapshot().editor.document.can_undo, "exactly one step");
}

#[test]
fn a_resolve_never_coalesces_with_a_slider_burst() {
    let (mut s, _) = ready(None);
    s.dispatch(Command::Edit(EditIntent::SetExposure(1.0)))
        .unwrap();
    let effect = apply_copied(&mut s, exposed(3.0));
    done(&mut s, &effect, resolved(exposed(3.0)));
    s.dispatch(Command::Edit(EditIntent::Undo)).unwrap();
    assert_eq!(exposure(&s), 1.0);
}

#[test]
fn a_resolve_for_a_moved_revision_is_dropped_without_error() {
    let (mut s, events) = ready(None);
    let effect = apply_copied(&mut s, exposed(2.0));
    s.dispatch(Command::Edit(EditIntent::SetExposure(1.0)))
        .unwrap();
    done(&mut s, &effect, resolved(exposed(2.0)));
    assert_eq!(exposure(&s), 1.0, "the newer edit stands");
    assert_eq!(s.snapshot().error, None);
    assert_eq!(
        events.0.lock().unwrap().last(),
        Some(&Notice::SettingsDropped)
    );
}

#[test]
fn a_resolve_for_a_replaced_session_is_dropped() {
    let (mut s, _) = ready(None);
    let effect = apply_copied(&mut s, exposed(2.0));
    select(&mut s, 2, None);
    // The selection change cleared nothing from `pending`, so the late result still completes.
    s.complete(Completion {
        token: effect.token,
        result: resolved(exposed(2.0)),
    })
    .unwrap();
    assert_eq!(exposure(&s), 0.0, "image 2 is untouched");
}

#[test]
fn a_failed_resolve_reports_the_error_and_changes_nothing() {
    let (mut s, _) = ready(None);
    let effect = apply_copied(&mut s, exposed(2.0));
    done(
        &mut s,
        &effect,
        Err(AppError::InvalidCommand("preset not found".into())),
    );
    assert_eq!(exposure(&s), 0.0);
    assert_eq!(
        s.snapshot().error,
        Some(AppError::InvalidCommand("preset not found".into()))
    );
}

#[test]
fn apply_settings_is_validated_and_needs_an_editable_document() {
    let (mut s, _) = ready(None);
    for (source, amount) in [
        (SettingsSource::Preset(1), 1.5),
        (SettingsSource::Preset(1), -0.1),
        (SettingsSource::Preset(1), f32::NAN),
        (SettingsSource::Preset(0), 1.0),
        (SettingsSource::Snapshot(-2), 1.0),
    ] {
        assert!(
            matches!(
                s.dispatch(Command::ApplySettings {
                    source,
                    amount,
                    replace_all: false
                }),
                Err(AppError::InvalidCommand(_))
            ),
            "amount {amount}"
        );
    }
    let mut closed =
        ApplicationSession::new(Arc::new(Clk), Arc::new(Events::default()), Arc::new(Render));
    let open = closed
        .dispatch(Command::OpenCatalog("c".into()))
        .unwrap()
        .remove(0);
    done(&mut closed, &open, Ok(Outcome::Opened));
    assert!(closed
        .dispatch(Command::ApplySettings {
            source: SettingsSource::Preset(1),
            amount: 1.0,
            replace_all: false
        })
        .is_err());
}

#[test]
fn revert_returns_to_the_opened_params_as_its_own_step() {
    let (mut s, _) = ready(Some(exposed(1.5)));
    s.dispatch(Command::Edit(EditIntent::SetExposure(2.0)))
        .unwrap();
    s.dispatch(Command::Edit(EditIntent::RevertToOpened))
        .unwrap();
    assert_eq!(exposure(&s), 1.5);
    s.dispatch(Command::Edit(EditIntent::Undo)).unwrap();
    assert_eq!(exposure(&s), 2.0);
}

#[test]
fn reset_returns_to_defaults_and_is_undoable() {
    let (mut s, _) = ready(Some(exposed(1.5)));
    s.dispatch(Command::Edit(EditIntent::ResetToDefault))
        .unwrap();
    assert_eq!(exposure(&s), 0.0);
    s.dispatch(Command::Edit(EditIntent::Undo)).unwrap();
    assert_eq!(exposure(&s), 1.5);
}

fn snapshots(names: &[(i64, &str)]) -> Vec<SnapshotSummary> {
    names
        .iter()
        .map(|(id, name)| SnapshotSummary {
            id: *id,
            name: (*name).into(),
            created_at: 0,
        })
        .collect()
}

fn pending_snapshot_load(effects: &[Effect]) -> Option<&Effect> {
    effects
        .iter()
        .find(|e| matches!(e.operation, Operation::LoadSnapshots(_)))
}

#[test]
fn selecting_an_image_loads_its_snapshots_and_stale_lists_are_dropped() {
    let (mut s, _) = ready(None);
    let first = select(&mut s, 2, None);
    let for_two = pending_snapshot_load(&first).unwrap().clone();
    assert_eq!(for_two.operation, Operation::LoadSnapshots(ImageId(2)));
    // Image 3 is selected before image 2's list arrives: it must not label image 3.
    let third = s.dispatch(Command::SelectImage(Some(ImageId(3)))).unwrap();
    done(
        &mut s,
        &for_two,
        Ok(Outcome::Snapshots(snapshots(&[(1, "a")]))),
    );
    assert!(s.snapshot().editor.snapshots.is_empty());
    // Once image 3's own row arrives its request replaces any older one.
    let again = done(
        &mut s,
        &third[0],
        Ok(Outcome::Image(Some(Box::new(row(3))))),
    );
    assert_eq!(
        pending_snapshot_load(&again).unwrap().operation,
        Operation::LoadSnapshots(ImageId(3))
    );
}

#[test]
fn snapshot_mutations_validate_and_reload_the_list() {
    let (mut s, _) = ready(None);
    let load = s.dispatch(Command::LoadSnapshots(ImageId(1))).unwrap();
    done(
        &mut s,
        &load[0],
        Ok(Outcome::Snapshots(snapshots(&[(4, "old")]))),
    );
    assert_eq!(s.snapshot().editor.snapshots.len(), 1);
    let tags_before = s.snapshot().library.tags_revision;

    let refused = |s: &mut ApplicationSession, m: Mutation| {
        assert!(
            matches!(
                s.dispatch(Command::Mutate(m)),
                Err(AppError::EditRefused(_))
            ),
            "empty names are refused"
        );
    };
    refused(
        &mut s,
        Mutation::CreateSnapshot {
            image: ImageId(1),
            name: "  ".into(),
            params: Arc::new(exposed(0.0)),
        },
    );
    refused(
        &mut s,
        Mutation::RenameSnapshot {
            id: 4,
            name: String::new(),
        },
    );

    for (mutation, report) in [
        (
            Mutation::CreateSnapshot {
                image: ImageId(1),
                name: "warm".into(),
                params: Arc::new(exposed(1.0)),
            },
            MutationReport::SnapshotCreated(9),
        ),
        (
            Mutation::RenameSnapshot {
                id: 4,
                name: "new".into(),
            },
            MutationReport::Done,
        ),
        (Mutation::DeleteSnapshot(4), MutationReport::Done),
    ] {
        let effect = s.dispatch(Command::Mutate(mutation)).unwrap().remove(0);
        let follow = done(&mut s, &effect, Ok(Outcome::Mutated(report)));
        assert_eq!(follow.len(), 1, "only the list is re-read");
        let reload = pending_snapshot_load(&follow).unwrap();
        assert_eq!(reload.operation, Operation::LoadSnapshots(ImageId(1)));
        done(
            &mut s,
            reload,
            Ok(Outcome::Snapshots(snapshots(&[(9, "warm")]))),
        );
    }
    assert_eq!(s.snapshot().editor.snapshots[0].name, "warm");
    assert_eq!(s.snapshot().library.tags_revision, tags_before);
}

#[test]
fn opening_the_catalog_loads_the_presets() {
    let mut s =
        ApplicationSession::new(Arc::new(Clk), Arc::new(Events::default()), Arc::new(Render));
    let open = s
        .dispatch(Command::OpenCatalog("c".into()))
        .unwrap()
        .remove(0);
    let follow = done(&mut s, &open, Ok(Outcome::Opened));
    let load = follow
        .iter()
        .find(|e| e.operation == Operation::LoadPresets)
        .expect("LoadPresets follows Opened");
    let preset = PresetSummary {
        id: 1,
        name: "Punchy".into(),
        group_name: "Built-in".into(),
        builtin: true,
        ..Default::default()
    };
    done(&mut s, load, Ok(Outcome::Presets(vec![preset.clone()])));
    assert_eq!(*s.snapshot().library.presets, vec![preset]);
}
