//! Deterministic tests for the pure autosave coordinator (no threads, no clocks).

use core_app::autosave::{
    Autosave, EditOrigin, FlushTarget, SaveKey, SaveSeq, IDLE_MS, MAX_WAIT_MS, RETRY_BACKOFF_MS,
};
use core_model::identity::{
    CatalogEpoch, CatalogRevision, EditRevision, EditorSession, ImageId, ProcessVersion,
};
use core_model::DevelopParams;
use std::sync::Arc;

fn key(n: i64) -> SaveKey {
    key_in(1, n)
}

fn key_in(epoch: u64, n: i64) -> SaveKey {
    SaveKey {
        catalog_epoch: CatalogEpoch(epoch),
        image_id: ImageId(n),
        content_hash: format!("{n:064x}"),
    }
}

fn org(session: u64, revision: u64) -> EditOrigin {
    EditOrigin {
        session: EditorSession(session),
        revision: EditRevision(revision),
    }
}

fn seq(n: u64) -> SaveSeq {
    SaveSeq(n)
}

fn cat(n: i64) -> Result<CatalogRevision, core_app::autosave::SaveFailure> {
    Ok(CatalogRevision(n))
}

fn pv() -> ProcessVersion {
    ProcessVersion(7)
}

fn params(exposure: f32) -> Arc<DevelopParams> {
    Arc::new(DevelopParams {
        exposure,
        ..DevelopParams::default()
    })
}

#[test]
fn idle_debounce_250ms() {
    let mut a = Autosave::new();
    assert!(a.record(key(1), org(1, 1), params(0.5), pv(), 0));
    assert!(a.due(249).is_empty(), "nothing due before the idle window");
    let reqs = a.due(250);
    assert_eq!(reqs.len(), 1);
    assert_eq!((reqs[0].key.clone(), reqs[0].seq), (key(1), seq(1)));
    assert_eq!(reqs[0].origin, org(1, 1));
    assert!((reqs[0].params.exposure - 0.5).abs() < f32::EPSILON);
    assert_eq!(reqs[0].process_version, pv());
    assert_eq!(a.next_deadline(), None, "in flight: nothing scheduled");
}

#[test]
fn max_wait_ceiling_at_1000ms() {
    let mut a = Autosave::new();
    // Continuous editing every 100 ms: the idle window never elapses.
    for step in 0..10u64 {
        let t = step * 100;
        assert!(a.record(key(1), org(1, step + 1), params(step as f32), pv(), t));
        assert!(a.due(t).is_empty(), "no save before the MAX_WAIT ceiling");
    }
    assert_eq!(a.next_deadline(), Some(MAX_WAIT_MS));
    let reqs = a.due(MAX_WAIT_MS);
    assert_eq!(reqs.len(), 1, "one save despite ten records");
    assert_eq!(reqs[0].seq, seq(10), "latest-wins at the ceiling");
    assert_eq!(reqs[0].origin, org(1, 10));
}

#[test]
fn latest_wins_collapses_intermediate_seqs() {
    let mut a = Autosave::new();
    for i in 1..=5u64 {
        assert!(a.record(key(1), org(1, i), params(i as f32), pv(), (i - 1) * 10));
    }
    let reqs = a.due(40 + IDLE_MS);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].seq, seq(5));
    assert!((reqs[0].params.exposure - 5.0).abs() < f32::EPSILON);
    assert_eq!(reqs[0].touch_count, 5);
}

#[test]
fn per_key_independence() {
    let mut a = Autosave::new();
    assert!(a.record(key(1), org(1, 1), params(1.0), pv(), 0));
    assert!(a.record(key(2), org(1, 1), params(2.0), pv(), 200));
    let reqs = a.due(250);
    assert_eq!(reqs.len(), 1, "A is due at 250 regardless of B");
    assert_eq!(reqs[0].key, key(1));
    assert_eq!(
        a.next_deadline(),
        Some(450),
        "B keeps its own idle deadline from its own record"
    );
    let reqs = a.due(450);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].key, key(2));
}

#[test]
fn serialized_one_inflight_per_key() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.record(key(1), org(1, 2), params(2.0), pv(), 50);
    let reqs = a.due(300); // idle from last record (50 + 250)
    assert_eq!(reqs[0].seq, seq(2), "seq 2 in flight");
    a.record(key(1), org(1, 3), params(3.0), pv(), 310);
    assert!(
        a.due(350).is_empty(),
        "no second save while seq 2 is in flight"
    );
    assert!(a.completed(&key(1), seq(2), cat(1), 350).is_some());
    // seq 3 rides its own timer: idle 310+250=560 (beats max-wait 0+1000).
    assert!(a.due(559).is_empty());
    let reqs = a.due(560);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].seq, seq(3));
}

#[test]
fn stray_completion_acknowledges_nothing() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.record(key(1), org(1, 2), params(2.0), pv(), 10);
    let reqs = a.due(260); // seq 2 in flight
    assert_eq!(reqs[0].seq, seq(2));
    a.record(key(1), org(1, 3), params(3.0), pv(), 270);
    // A completion that is not the in-flight seq cannot acknowledge (or regress) anything.
    assert!(a.completed(&key(1), seq(3), cat(9), 300).is_none());
    assert!(a.completed(&key(1), seq(1), cat(9), 300).is_none());
    let p = a.pending();
    assert_eq!(p[0].saved, None);
    let ack = a.completed(&key(1), seq(2), cat(4), 310).unwrap();
    assert_eq!(ack.seq, seq(2));
    assert_eq!(ack.catalog_revision, CatalogRevision(4));
    assert_eq!(ack.origin, org(1, 2), "older seq keeps its own origin");
    assert_eq!(a.pending()[0].saved, Some(seq(2)), "seq 3 still dirty");
}

#[test]
fn failure_backoff_and_visible_error() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    assert_eq!(a.due(250).len(), 1, "first attempt on the idle timer");

    a.completed(&key(1), seq(1), Err("io".into()), 250);
    assert!(a.due(749).is_empty(), "first backoff step 500 not elapsed");
    assert_eq!(a.due(750).len(), 1);

    a.completed(&key(1), seq(1), Err("io".into()), 750);
    assert!(a.due(2249).is_empty());
    assert_eq!(a.due(2250).len(), 1, "second backoff step 1500");

    a.completed(&key(1), seq(1), Err("io".into()), 2250);
    assert!(a.due(6249).is_empty());
    assert_eq!(a.due(6250).len(), 1, "third backoff step 4000");

    a.completed(&key(1), seq(1), Err("io".into()), 6250);
    assert_eq!(
        a.visible_error().unwrap(),
        "Changes haven't been saved. Darkroom will keep trying."
    );
    assert_eq!(RETRY_BACKOFF_MS, [500, 1500, 4000]);
    let p = a.pending();
    assert_eq!(p[0].failures, 4);
    assert_eq!(p[0].last_error.as_deref(), Some("io"));
    assert_eq!(a.failure(&key(1)), Some((4, "io".into())));
    assert!(a.due(10249).is_empty());

    // Keep retrying every 4000 ms; success clears the error and prunes the entry.
    assert_eq!(a.due(10250).len(), 1);
    assert!(a.completed(&key(1), seq(1), cat(1), 10250).is_some());
    assert_eq!(a.visible_error(), None);
    assert!(a.pending().is_empty());
    assert!(a.is_idle());
}

#[test]
fn record_during_backoff_does_not_shorten_it() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.due(250);
    a.completed(&key(1), seq(1), Err("io".into()), 250);
    // Editing during the 500 ms backoff must not dispatch earlier (paced retries).
    a.record(key(1), org(1, 2), params(2.0), pv(), 300);
    assert!(a.due(300).is_empty());
    assert_eq!(a.next_deadline(), Some(750));
    let reqs = a.due(750);
    assert_eq!(reqs[0].seq, seq(2), "retry carries the latest params");
}

#[test]
fn flush_target_is_fixed_at_capture() {
    let mut a = Autosave::new();
    for i in 1..=5u64 {
        a.record(key(10), org(1, i), params(i as f32), pv(), i * 10);
    }
    a.record(key(20), org(1, 2), params(9.0), pv(), 10);
    let target = a.flush_target();
    assert_eq!(
        target,
        FlushTarget(vec![(key(10), seq(5)), (key(20), seq(6))])
    );

    let reqs = a.due_now();
    assert_eq!(reqs.len(), 2, "flush dispatches every dirty key");
    // A newer edit lands while saves are in flight.
    a.record(key(10), org(1, 6), params(6.0), pv(), 20);
    a.completed(&key(10), seq(5), cat(1), 30);
    assert!(!a.is_flushed(&target), "B still unsaved");
    a.completed(&key(20), seq(6), cat(1), 40);
    assert!(
        a.is_flushed(&target),
        "target is fixed: A@5 and B@6 landed even though A is dirty again at seq 7"
    );
}

#[test]
fn stop_admission_then_record_is_ignored() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.stop_admission();
    assert!(!a.record(key(2), org(1, 1), params(2.0), pv(), 10));
    assert!(!a.record(key(1), org(1, 2), params(1.5), pv(), 10));
    // Already-recorded work still drains.
    let reqs = a.due_now();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].key, key(1));
    a.completed(&key(1), seq(1), cat(1), 20);
    assert!(a.is_idle());
    // Admission can be re-opened after a cancelled shutdown.
    a.resume_admission();
    assert!(a.record(key(2), org(1, 1), params(2.0), pv(), 30));
}

#[test]
fn discard_never_removes_an_inflight_key_and_never_counts_as_flushed() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.record(key(2), org(1, 2), params(2.0), pv(), 0);
    let target = a.flush_target();
    assert_eq!(target.0.len(), 2);
    a.due(250); // both in flight
    let refused = a.discard(&[key(1), key(2)]);
    assert_eq!(refused, vec![key(1), key(2)], "in-flight keys are kept");
    a.completed(&key(1), seq(1), cat(1), 260);
    a.record(key(3), org(1, 1), params(3.0), pv(), 270);
    assert!(
        a.discard(&[key(3)]).is_empty(),
        "idle dirty key is discardable"
    );
    let after = FlushTarget(vec![(key(3), seq(3))]);
    assert!(
        !a.is_flushed(&after),
        "a discarded key was never acknowledged, so it is not flushed"
    );
    assert!(!a.is_flushed(&target), "key 2 still in flight");
    a.completed(&key(2), seq(2), cat(1), 300);
    assert!(
        a.is_flushed(&target),
        "acks recorded for both survive pruning"
    );
    assert!(a.is_idle());
}

#[test]
fn source_changed_blocks_the_entry_and_it_is_never_retried() {
    use core_app::autosave::SaveFailure;
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.due(250);
    a.completed(
        &key(1),
        seq(1),
        Err(SaveFailure::SourceChanged("gone".into())),
        250,
    );
    assert!(a.is_blocked(&key(1)));
    assert_eq!(a.blocked_keys(), vec![key(1)]);
    assert!(a.due(1_000_000).is_empty(), "no automatic retry, ever");
    assert!(a.due_now().is_empty());
    assert_eq!(a.next_deadline(), None);
    assert!(!a.is_idle(), "still dirty");
    assert_eq!(
        a.discard(&[key(1)]),
        vec![],
        "idle blocked key is discardable"
    );
    assert!(a.is_idle());
}

#[test]
fn retain_epoch_refuses_to_drop_unsaved_keys() {
    let mut a = Autosave::new();
    a.record(key_in(1, 5), org(1, 1), params(1.0), pv(), 0);
    assert_eq!(a.retain_epoch(CatalogEpoch(2)), Err(vec![key_in(1, 5)]));
    assert_eq!(a.pending().len(), 1, "nothing was dropped");
    a.due(250);
    a.completed(&key_in(1, 5), seq(1), cat(1), 260);
    assert_eq!(a.retain_epoch(CatalogEpoch(2)), Ok(()));
}

#[test]
fn completion_matching_no_inflight_save_is_stale() {
    use core_app::autosave::Completed;
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    assert_eq!(
        a.completed_outcome(&key(1), seq(1), Err("x".into()), 10),
        Completed::Stale,
        "nothing dispatched yet"
    );
    a.due(250);
    assert_eq!(
        a.completed_outcome(&key(1), seq(9), Err("x".into()), 260),
        Completed::Stale
    );
    assert_eq!(
        a.completed_outcome(&key(1), seq(1), Err("x".into()), 260),
        Completed::Failed
    );
}

#[test]
fn touch_count_across_an_inflight_save() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    let reqs = a.due(250);
    assert_eq!(reqs[0].touch_count, 1);
    // An edit arrives while the save is in flight.
    a.record(key(1), org(1, 2), params(2.0), pv(), 300);
    a.completed(&key(1), seq(1), cat(1), 400);
    let reqs = a.due(550); // idle from the second record: 300 + 250
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].seq, seq(2));
    assert_eq!(
        reqs[0].touch_count, 1,
        "legacy `touchCount - touches`: the dispatched touch was subtracted"
    );
    a.completed(&key(1), seq(2), cat(2), 560);
    assert!(a.is_idle());
}

#[test]
fn equal_params_assign_no_seq_and_unknown_completions_are_safe() {
    let mut a = Autosave::new();
    assert!(a.record(key(1), org(1, 3), params(1.0), pv(), 0));
    assert!(
        !a.record(key(1), org(1, 4), params(1.0), pv(), 10),
        "equal params: no new seq"
    );
    // Completions for keys/seqs we never tracked: ignored, no panic.
    assert!(a.completed(&key(99), seq(7), cat(1), 20).is_none());
    assert!(a
        .completed(&key(99), seq(7), Err("nope".into()), 20)
        .is_none());
    assert!(a
        .completed(&key(1), seq(9), Err("stray".into()), 20)
        .is_none());
    assert_eq!(a.due(250).len(), 1);
    assert!(a.completed(&key(1), seq(1), cat(1), 260).is_some());
    // Duplicate of the same ack afterwards: entry already pruned, ignored.
    assert!(a.completed(&key(1), seq(1), cat(1), 270).is_none());
    assert!(a.is_idle());
}

#[test]
fn clean_entry_is_pruned_and_new_episode_reanchors_max_wait() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.due(250);
    a.completed(&key(1), seq(1), cat(1), 260);
    assert!(a.is_idle());
    assert!(a.pending().is_empty(), "clean entry removed from the map");
    assert_eq!(a.next_deadline(), None);
    // A fresh edit starts a new dirty episode with its own MAX_WAIT anchor.
    assert!(a.record(key(1), org(1, 2), params(2.0), pv(), 5000));
    assert_eq!(a.next_deadline(), Some(5250));
    a.due_now();
    a.completed(&key(1), seq(2), cat(2), 5100);
    assert!(a.is_idle());
}

#[test]
fn due_now_ignores_all_timers_but_keeps_serialization() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.record(key(2), org(1, 1), params(2.0), pv(), 1);
    let reqs = a.due_now();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
        vec![key(1), key(2)],
        "deterministic order"
    );
    assert!(
        a.due_now().is_empty(),
        "still one in flight per key even for flush"
    );
    a.completed(&key(1), seq(1), cat(1), 5);
    a.completed(&key(2), seq(2), cat(1), 5);
    assert!(a.is_idle());
}

// --- save identity across editor sessions and catalog epochs ---------------------------------

#[test]
fn reopened_session_revision_one_is_still_dirty_relative_to_saved_seq() {
    let mut a = Autosave::new();
    // Session 1 reaches revision 7 and is saved.
    a.record(key(1), org(1, 7), params(1.0), pv(), 0);
    a.due(250);
    assert!(a.completed(&key(1), seq(1), cat(1), 260).is_some());
    // Session 2 restarts at revision 1 with different params: it must NOT compare below the old
    // revision 7 and be ignored.
    assert!(a.record(key(1), org(2, 1), params(2.0), pv(), 1000));
    let target = a.flush_target();
    assert_eq!(target.0, vec![(key(1), seq(2))]);
    let reqs = a.due(1250);
    assert_eq!((reqs[0].seq, reqs[0].origin), (seq(2), org(2, 1)));
}

#[test]
fn delayed_completion_of_old_session_does_not_acknowledge_the_new_session() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 3), params(1.0), pv(), 0);
    let reqs = a.due(250); // session 1's seq 1 in flight
    assert_eq!(reqs[0].origin, org(1, 3));
    // Session 1 closes, session 2 opens and edits to different params while seq 1 is in flight.
    a.record(key(1), org(2, 1), params(2.0), pv(), 300);
    assert!(a.due(600).is_empty(), "serialized across reopen");
    let ack = a.completed(&key(1), seq(1), cat(1), 700).unwrap();
    assert_eq!(
        ack.origin,
        org(1, 3),
        "the ack names session 1, so session 2 is not marked saved"
    );
    assert_eq!(a.pending()[0].saved, Some(seq(1)));
    assert!(!a.is_idle(), "session 2's seq 2 is still dirty");
}

#[test]
fn two_saves_for_one_key_stay_ordered_across_reopen() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 2), params(1.0), pv(), 0);
    let first = a.due(250);
    a.record(key(1), org(2, 1), params(2.0), pv(), 260);
    a.record(key(1), org(2, 2), params(3.0), pv(), 270);
    assert!(a.due_now().is_empty(), "never two in flight for one key");
    a.completed(&key(1), first[0].seq, cat(1), 300);
    let second = a.due_now();
    assert_eq!(second.len(), 1);
    assert!(second[0].seq > first[0].seq);
    assert_eq!(second[0].origin, org(2, 2));
    assert!((second[0].params.exposure - 3.0).abs() < f32::EPSILON);
}

#[test]
fn restored_pending_rebinds_the_latest_seq_to_the_new_session() {
    let mut a = Autosave::new();
    a.record(key(1), org(1, 3), params(1.0), pv(), 0);
    a.due(250); // in flight
                // Session 2 restores the same unsaved params: no new seq, but the ack must reach session 2.
    assert!(!a.record(key(1), org(2, 2), params(1.0), pv(), 300));
    let ack = a.completed(&key(1), seq(1), cat(5), 400).unwrap();
    assert_eq!(ack.origin, org(2, 2));
    assert!(a.is_idle());
}

#[test]
fn different_catalog_epochs_are_independent_keys() {
    let mut a = Autosave::new();
    a.record(key_in(1, 5), org(1, 1), params(1.0), pv(), 0);
    a.record(key_in(2, 5), org(1, 1), params(1.0), pv(), 0);
    let reqs = a.due(250);
    assert_eq!(reqs.len(), 2, "same image id, two epochs, two saves");
    // A completion for the old epoch cannot acknowledge the new epoch's key.
    assert!(a
        .completed(&key_in(1, 5), reqs[0].seq, cat(1), 300)
        .is_some());
    assert!(
        a.completed(&key_in(1, 5), reqs[1].seq, cat(1), 300)
            .is_none(),
        "wrong key for that seq"
    );
    assert_eq!(a.pending().len(), 1);
    assert_eq!(a.pending()[0].key.catalog_epoch, CatalogEpoch(2));
    assert!(
        a.retain_epoch(CatalogEpoch(3)).is_err(),
        "epoch 2 is unsaved"
    );
    a.completed(&key_in(2, 5), reqs[1].seq, cat(1), 310);
    assert_eq!(a.retain_epoch(CatalogEpoch(3)), Ok(()));
    assert!(a.pending().is_empty());
}

#[test]
fn a_replaced_source_file_is_a_different_key() {
    let mut a = Autosave::new();
    let mut other = key(1);
    other.content_hash = "ff".repeat(32);
    a.record(key(1), org(1, 1), params(1.0), pv(), 0);
    a.record(other.clone(), org(2, 1), params(1.0), pv(), 0);
    assert_eq!(a.due(250).len(), 2);
    assert_eq!(a.latest_unsaved(&other).map(|(p, _)| p.exposure), Some(1.0));
}
