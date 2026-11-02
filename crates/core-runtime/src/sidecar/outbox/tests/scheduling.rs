//! Scheduling half of the outbox tests: what time does to the queue — restart recovery,
//! report counts, wake-up pacing, and supersession. Shared helpers live in `mod.rs`.
use super::*;
use std::sync::atomic::AtomicUsize;

#[test]
fn an_intent_left_by_the_previous_run_is_written_on_the_next_one() {
    let f = Fix::new("outbox-restart");
    let a = f.insert_image(3, "a.cr3");
    save(&f, a);
    // The previous run: the card was not writable, so the intent stayed queued (and backed off).
    let clock = FakeClock::new();
    drain(&f, &denied, &clock);
    assert!(f.sidecar("a.cr3").is_none());

    // A restart loses every in-memory queue; the catalog is all that is left. The worker
    // resets the schedule at start, so the backed-off row is retried immediately.
    let reopened = f.reopened();
    reset_outbox_schedule(&reopened.db.lock().unwrap().conn, None).unwrap();
    assert_eq!(
        row(&reopened, a).attempts,
        1,
        "attempts survive the restart"
    );
    let report = drain_once(
        &reopened.gate,
        &reopened.db,
        &production_write,
        &clock,
        &never,
    )
    .unwrap();
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert!(
        f.sidecar("a.cr3").is_some(),
        "the restart wrote the sidecar"
    );
    assert!(owed(&reopened).is_empty());
    assert_eq!(
        mirrored_revision(&reopened.db.lock().unwrap().conn, a).unwrap(),
        Some((1, 1))
    );
}

#[test]
fn pending_report_counts_what_is_owed_and_names_the_failure() {
    let f = Fix::new("outbox-report");
    let a = f.insert_image(5, "a.cr3");
    let b = f.insert_image(6, "b.cr3");
    save(&f, a);
    save(&f, b);
    let handle = OutboxHandle::new(f.db.clone());
    // Committed, never attempted: owed, but nothing has failed yet.
    assert_eq!(handle.pending_report().0, 2);
    assert_eq!(handle.pending_report().1, 0);
    assert_eq!(handle.pending_report().2, None);

    let clock = FakeClock::new();
    drain(&f, &denied, &clock);
    let (pending, failing, last_error) = handle.pending_report();
    assert_eq!(pending, 2, "a failed write still owes a sidecar");
    assert_eq!(failing, 2, "both attempts hit the filesystem");
    assert!(
        last_error.as_deref().is_some_and(|e| e.contains("denied")),
        "{last_error:?}"
    );

    // The volume comes back and the backoff elapses: settling removes them from the report.
    clock.advance(BACKOFF_CAP);
    drain(&f, &production_write, &clock);
    assert_eq!(handle.pending_report(), (0, 0, None));
}

#[test]
fn wakeups_before_eligibility_attempt_nothing_and_time_makes_rows_due_again() {
    let f = Fix::new("outbox-backoff");
    let a = f.insert_image(1, "a.cr3");
    save(&f, a);
    let clock = FakeClock::new();
    let calls = AtomicUsize::new(0);
    let write = |w: &SidecarWrite| {
        calls.fetch_add(1, Ordering::SeqCst);
        denied(w)
    };
    for _ in 0..5 {
        drain(&f, &write, &clock);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1, "attempted exactly once");
    assert_eq!(row(&f, a).attempts, 1);

    // Past the first backoff the row is due again.
    clock.advance(backoff(1));
    drain(&f, &write, &clock);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(row(&f, a).attempts, 2);
    // ...and the second backoff is longer.
    clock.advance(backoff(1));
    drain(&f, &write, &clock);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "backoff doubled");
}

#[test]
fn a_handle_pass_before_the_due_time_drains_nothing() {
    let f = Fix::new("outbox-pass");
    let a = f.insert_image(1, "a.cr3");
    save(&f, a);
    let clock = Arc::new(FakeClock::new());
    let handle = OutboxHandle::with_clock(f.db.clone(), clock.clone());
    let calls = AtomicUsize::new(0);
    let write = |w: &SidecarWrite| {
        calls.fetch_add(1, Ordering::SeqCst);
        denied(w)
    };
    // The fresh intent is due: first pass attempts it, then asks to wait out the backoff.
    let wait = handle.pass(&f.gate, &write, &never, &|| {});
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(wait <= POLL && wait > Duration::ZERO);
    for _ in 0..3 {
        handle.notify();
        handle.pass(&f.gate, &write, &never, &|| {});
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "wakes before due are inert"
    );
    clock.advance(backoff(1));
    handle.pass(&f.gate, &write, &never, &|| {});
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn a_superseding_save_is_due_immediately_and_not_completed_by_the_old_write() {
    let f = Fix::new("outbox-supersede");
    let a = f.insert_image(1, "a.cr3");
    save(&f, a);
    let clock = FakeClock::new();
    drain(&f, &denied, &clock);
    let backed_off = row(&f, a);
    assert!(backed_off.next_attempt_at > clock.now_ms());
    assert_eq!(backed_off.attempts, 1);

    // A new save supersedes the backed-off intent: due now, attempts back to zero.
    let revision = save(&f, a);
    let fresh = row(&f, a);
    assert_eq!(fresh.edit_revision, revision);
    assert_eq!((fresh.next_attempt_at, fresh.attempts), (0, 0));

    // A write of the OLD revision that lands after a newer save cannot settle the new intent:
    // the in-fence re-read sees different params (Retry) and the deferral is revision-guarded.
    let racing_save = |w: &SidecarWrite| {
        save(&f, a);
        production_write(w)
    };
    drain(&f, &racing_save, &clock);
    let after = row(&f, a);
    assert_eq!(
        after.edit_revision,
        revision + 1,
        "the newer intent survives"
    );
    assert_eq!(after.next_attempt_at, 0, "and is still due now");
    let report = drain(&f, &production_write, &clock);
    assert_eq!(report.mirrored, 1, "{report:?}");
    assert!(no_temp_files(&f));
}
