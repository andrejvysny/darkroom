//! Deterministic tests for the pure autosave coordinator (no threads, no clocks).

use core_app::autosave::{Autosave, FlushTarget, IDLE_MS, MAX_WAIT_MS, RETRY_BACKOFF_MS};
use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;
use std::sync::Arc;

fn img(n: i64) -> ImageId {
    ImageId(n)
}

fn rev(n: u64) -> EditRevision {
    EditRevision(n)
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
    assert!(a.record(img(1), rev(1), params(0.5), pv(), 0));
    assert!(a.due(249).is_empty(), "nothing due before the idle window");
    let reqs = a.due(250);
    assert_eq!(reqs.len(), 1);
    assert_eq!((reqs[0].image, reqs[0].revision), (img(1), rev(1)));
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
        assert!(a.record(img(1), rev(step + 1), params(step as f32), pv(), t));
        assert!(a.due(t).is_empty(), "no save before the MAX_WAIT ceiling");
    }
    assert_eq!(a.next_deadline(), Some(MAX_WAIT_MS));
    let reqs = a.due(MAX_WAIT_MS);
    assert_eq!(reqs.len(), 1, "one save despite ten records");
    assert_eq!(reqs[0].revision, rev(10), "latest-wins at the ceiling");
}

#[test]
fn latest_wins_collapses_intermediate_revisions() {
    let mut a = Autosave::new();
    for i in 1..=5u64 {
        assert!(a.record(img(1), rev(i), params(i as f32), pv(), (i - 1) * 10));
    }
    let reqs = a.due(40 + IDLE_MS);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].revision, rev(5));
    assert!((reqs[0].params.exposure - 5.0).abs() < f32::EPSILON);
    assert_eq!(reqs[0].touch_count, 5);
}

#[test]
fn per_image_independence() {
    let mut a = Autosave::new();
    assert!(a.record(img(1), rev(1), params(1.0), pv(), 0));
    assert!(a.record(img(2), rev(1), params(2.0), pv(), 200));
    let reqs = a.due(250);
    assert_eq!(reqs.len(), 1, "A is due at 250 regardless of B");
    assert_eq!(reqs[0].image, img(1));
    assert_eq!(
        a.next_deadline(),
        Some(450),
        "B keeps its own idle deadline from its own record"
    );
    let reqs = a.due(450);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].image, img(2));
}

#[test]
fn serialized_one_inflight_per_image() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.record(img(1), rev(2), params(2.0), pv(), 50);
    let reqs = a.due(300); // idle from last record (50 + 250)
    assert_eq!(reqs[0].revision, rev(2), "r2 in flight");
    a.record(img(1), rev(3), params(3.0), pv(), 310);
    assert!(
        a.due(350).is_empty(),
        "no second save while r2 is in flight"
    );
    a.completed(img(1), rev(2), Ok(()), 350);
    // r3 rides its own timer: idle 310+250=560 (beats max-wait 0+1000).
    assert!(a.due(559).is_empty());
    let reqs = a.due(560);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].revision, rev(3));
}

#[test]
fn out_of_order_completion_never_regresses_ack() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.record(img(1), rev(2), params(2.0), pv(), 10);
    let reqs = a.due(260); // r2 in flight
    assert_eq!(reqs[0].revision, rev(2));
    a.record(img(1), rev(3), params(3.0), pv(), 270);
    // Host (mis)reports r3 saved while r2 is still its in-flight request: the ack applies
    // (that commit did land) and the stray r2 ack below cannot regress it.
    a.completed(img(1), rev(3), Ok(()), 300);
    let p = a.pending();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].saved, Some(rev(3)), "r3 acknowledged");
    a.completed(img(1), rev(2), Ok(()), 310);
    assert!(a.pending().is_empty(), "clean and drained, entry pruned");
    assert!(a.is_idle());
}

#[test]
fn failure_backoff_and_visible_error() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    assert_eq!(a.due(250).len(), 1, "first attempt on the idle timer");

    a.completed(img(1), rev(1), Err("io".into()), 250);
    assert!(a.due(749).is_empty(), "first backoff step 500 not elapsed");
    assert_eq!(a.due(750).len(), 1);

    a.completed(img(1), rev(1), Err("io".into()), 750);
    assert!(a.due(2249).is_empty());
    assert_eq!(a.due(2250).len(), 1, "second backoff step 1500");

    a.completed(img(1), rev(1), Err("io".into()), 2250);
    assert!(a.due(6249).is_empty());
    assert_eq!(a.due(6250).len(), 1, "third backoff step 4000");

    a.completed(img(1), rev(1), Err("io".into()), 6250);
    assert!(
        a.visible_error().is_some(),
        "backoff steps exhausted -> visible"
    );
    assert_eq!(
        a.visible_error().unwrap(),
        "Changes haven't been saved. Darkroom will keep trying."
    );
    assert_eq!(RETRY_BACKOFF_MS, [500, 1500, 4000]);
    let p = a.pending();
    assert_eq!(p[0].failures, 4);
    assert_eq!(p[0].last_error.as_deref(), Some("io"));
    assert!(a.due(10249).is_empty());

    // Keep retrying every 4000 ms; success clears the error and prunes the entry.
    assert_eq!(a.due(10250).len(), 1);
    a.completed(img(1), rev(1), Ok(()), 10250);
    assert_eq!(a.visible_error(), None);
    assert!(a.pending().is_empty());
    assert!(a.is_idle());
}

#[test]
fn record_during_backoff_does_not_shorten_it() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.due(250);
    a.completed(img(1), rev(1), Err("io".into()), 250);
    // Editing during the 500 ms backoff must not dispatch earlier (paced retries).
    a.record(img(1), rev(2), params(2.0), pv(), 300);
    assert!(a.due(300).is_empty());
    assert_eq!(a.next_deadline(), Some(750));
    let reqs = a.due(750);
    assert_eq!(reqs[0].revision, rev(2), "retry carries the latest params");
}

#[test]
fn flush_target_is_fixed_at_capture() {
    let mut a = Autosave::new();
    for i in 1..=5u64 {
        a.record(img(10), rev(i), params(i as f32), pv(), i * 10);
    }
    a.record(img(20), rev(2), params(9.0), pv(), 10);
    let target = a.flush_target();
    assert_eq!(
        target,
        FlushTarget(vec![(img(10), rev(5)), (img(20), rev(2))])
    );

    let reqs = a.due_now();
    assert_eq!(reqs.len(), 2, "flush dispatches every dirty image");
    // A newer edit lands while saves are in flight.
    a.record(img(10), rev(6), params(6.0), pv(), 20);
    a.completed(img(10), rev(5), Ok(()), 30);
    assert!(!a.is_flushed(&target), "B still unsaved");
    a.completed(img(20), rev(2), Ok(()), 40);
    assert!(
        a.is_flushed(&target),
        "target is fixed: A@5 and B@2 landed even though A is dirty again at r6"
    );
}

#[test]
fn stop_admission_then_record_is_ignored() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.stop_admission();
    assert!(!a.record(img(2), rev(1), params(2.0), pv(), 10));
    assert!(!a.record(img(1), rev(2), params(1.5), pv(), 10));
    // Already-recorded work still drains.
    let reqs = a.due_now();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].image, img(1));
    a.completed(img(1), rev(1), Ok(()), 20);
    assert!(a.is_idle());
}

#[test]
fn discard_is_an_explicit_user_decision() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.record(img(2), rev(2), params(2.0), pv(), 0);
    let target = a.flush_target();
    assert_eq!(target.0.len(), 2);
    a.due_now();
    a.discard(&[img(1)]);
    let p = a.pending();
    assert_eq!(p.len(), 1, "only the remaining image is pending");
    assert_eq!(p[0].image, img(2));
    assert!(
        !a.is_flushed(&target),
        "the remaining image is still unsaved"
    );
    a.completed(img(2), rev(2), Ok(()), 50);
    assert!(
        a.is_flushed(&target),
        "discarded A counts as satisfied once the remaining images land"
    );
    // A late completion for the discarded image is ignored safely.
    a.completed(img(1), rev(1), Err("io".into()), 60);
    assert!(a.is_idle());
}

#[test]
fn touch_count_across_an_inflight_save() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    let reqs = a.due(250);
    assert_eq!(reqs[0].touch_count, 1);
    // An edit arrives while the save is in flight.
    a.record(img(1), rev(2), params(2.0), pv(), 300);
    a.completed(img(1), rev(1), Ok(()), 400);
    let reqs = a.due(550); // idle from the r2 record: 300 + 250
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].revision, rev(2));
    assert_eq!(
        reqs[0].touch_count, 1,
        "legacy `touchCount - touches`: the dispatched touch was subtracted"
    );
    a.completed(img(1), rev(2), Ok(()), 560);
    assert!(a.is_idle());
}

#[test]
fn record_rejects_stale_revisions_and_unknown_completions_are_safe() {
    let mut a = Autosave::new();
    assert!(a.record(img(1), rev(3), params(1.0), pv(), 0));
    assert!(!a.record(img(1), rev(3), params(1.0), pv(), 10), "same rev");
    assert!(
        !a.record(img(1), rev(2), params(1.0), pv(), 10),
        "older rev"
    );
    // Completions for images/revisions we never tracked: ignored, no panic.
    a.completed(img(99), rev(7), Ok(()), 20);
    a.completed(img(99), rev(7), Err("nope".into()), 20);
    a.completed(img(1), rev(9), Err("stray".into()), 20);
    assert_eq!(a.due(250).len(), 1);
    a.completed(img(1), rev(3), Ok(()), 260);
    // Duplicate of the same ack afterwards: entry already pruned, ignored.
    a.completed(img(1), rev(3), Ok(()), 270);
    assert!(a.is_idle());
}

#[test]
fn clean_entry_is_pruned_and_new_episode_reanchors_max_wait() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.due(250);
    a.completed(img(1), rev(1), Ok(()), 260);
    assert!(a.is_idle());
    assert!(a.pending().is_empty(), "clean entry removed from the map");
    assert_eq!(a.next_deadline(), None);
    // A fresh edit starts a new dirty episode with its own MAX_WAIT anchor.
    assert!(a.record(img(1), rev(2), params(2.0), pv(), 5000));
    assert_eq!(a.next_deadline(), Some(5250));
    a.due_now();
    a.completed(img(1), rev(2), Ok(()), 5100);
    assert!(a.is_idle());
}

#[test]
fn due_now_ignores_all_timers_but_keeps_serialization() {
    let mut a = Autosave::new();
    a.record(img(1), rev(1), params(1.0), pv(), 0);
    a.record(img(2), rev(1), params(2.0), pv(), 1);
    let reqs = a.due_now();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        reqs.iter().map(|r| r.image).collect::<Vec<_>>(),
        vec![img(1), img(2)],
        "deterministic order"
    );
    assert!(
        a.due_now().is_empty(),
        "still one in flight per image even for flush"
    );
    a.completed(img(1), rev(1), Ok(()), 5);
    a.completed(img(2), rev(1), Ok(()), 5);
    assert!(a.is_idle());
}
