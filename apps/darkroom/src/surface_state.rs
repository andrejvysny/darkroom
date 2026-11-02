//! Bounded surface-acquire state machine.
//!
//! Deliberately free of `wgpu`/`winit` types: `window.rs` classifies
//! `wgpu::CurrentSurfaceTexture` variants into [`AcquireEvent`] and this module
//! decides what to do about it, so the retry/backoff policy is unit-testable
//! without a GPU or a window.

use std::time::{Duration, Instant};

/// One frame at 60 Hz: the first retry delay, matching a natural redraw cadence
/// so a transient hiccup costs at most one dropped frame.
const FIRST_RETRY: Duration = Duration::from_millis(16);
/// Exponential backoff is capped at 1 s: enough to stop a busy loop from
/// burning CPU/GPU while a surface is wedged, short enough to recover quickly
/// once the transient condition clears.
const MAX_BACKOFF: Duration = Duration::from_millis(1000);
/// Eight consecutive non-success events (≈16 ms + 32 ms + … + 1 s of waiting)
/// means the surface is not recovering on its own; declare failure instead of
/// retrying forever.
const MAX_STREAK: u32 = 8;
/// Surface recreation is cheap but not free; more than three recreations
/// without a single successful present indicates a device-level problem, which
/// this host does not recover from (device recreation is out of scope).
const MAX_RECREATIONS: u32 = 3;

/// Classification of an acquire attempt (or a window condition) free of wgpu types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireEvent {
    /// A presentable frame was acquired.
    Presented,
    /// A frame was acquired but the surface configuration is suboptimal.
    Suboptimal,
    /// Acquisition timed out; the compositor is slow or the surface is temporarily stalled.
    Timeout,
    /// The window is occluded; polling is pointless until it becomes visible again.
    Occluded,
    /// The surface configuration no longer matches the window; reconfigure.
    Outdated,
    /// The surface is lost and must be recreated.
    Lost,
    /// The acquisition failed validation; retrying cannot help.
    Validation,
    /// The window has zero size; rendering is suspended until it is resized.
    ZeroSize,
}

/// What the caller must do after an acquire attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Draw the acquired frame.
    Present,
    /// Reconfigure the surface, then retry after `after`.
    Reconfigure { after: Duration },
    /// Recreate the surface, then retry after `after`.
    RecreateSurface { after: Duration },
    /// Occluded: do not poll; wait for an `Occluded(false)`/`Resized` event.
    WaitForVisibility,
    /// Zero size: do not poll; wait for a `Resized` event.
    Suspend,
    /// Retry the acquisition after `after`.
    Retry { after: Duration },
    /// Stop rendering; surface an actionable error to the user.
    Fatal(SurfaceFailure),
}

/// Terminal surface failures; the host stops acquiring and shuts down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceFailure {
    /// The non-success streak hit [`MAX_STREAK`]; `last` names the event that
    /// finally exhausted the budget.
    RetriesExhausted { last: &'static str, attempts: u32 },
    /// Validation error; retries cannot help.
    Validation,
    /// An uncaptured wgpu device error (validation/OOM/internal) recorded by the
    /// `gpu::GpuErrors` hook instead of the default handler aborting the process;
    /// the message is the wgpu error text, surfaced to the user.
    UncapturedGpu(String),
    /// Surface recreation itself failed; the message is surfaced verbatim.
    RecreateFailed(String),
    /// The surface was lost too many times without ever presenting; this is a
    /// device-level loss, which this host reports instead of recovering.
    DeviceLost,
}

impl SurfaceFailure {
    /// User-facing text for a terminal failure; `streak` is the consecutive-failure count.
    pub fn message(&self, streak: u32) -> String {
        match self {
            SurfaceFailure::RetriesExhausted { last, attempts } => format!(
                "GPU surface acquisition failed {attempts} times in a row (last failure: {last}). \
                 Restart Darkroom; if this persists, check the display connection and GPU drivers."
            ),
            SurfaceFailure::Validation => "GPU surface validation error. \
                 Restart Darkroom; if this persists, check the display connection."
                .to_string(),
            SurfaceFailure::UncapturedGpu(message) => format!(
                "The GPU reported an error and Darkroom stopped rendering: {message}. \
                 Restart Darkroom; if this persists, check the display connection and GPU drivers."
            ),
            SurfaceFailure::RecreateFailed(message) => format!(
                "Failed to recreate the GPU surface after {streak} consecutive failures: \
                 {message}. Restart Darkroom; if this persists, check the display connection."
            ),
            SurfaceFailure::DeviceLost => format!(
                "GPU surface was lost {streak} times without ever presenting; this indicates a \
                 device-level GPU loss, which Darkroom cannot recover from at runtime. Restart \
                 Darkroom; if this persists, check the display connection and GPU drivers."
            ),
        }
    }
}

fn label(event: AcquireEvent) -> &'static str {
    match event {
        AcquireEvent::Presented => "presented",
        AcquireEvent::Suboptimal => "suboptimal",
        AcquireEvent::Timeout => "timeout",
        AcquireEvent::Occluded => "occluded",
        AcquireEvent::Outdated => "outdated",
        AcquireEvent::Lost => "lost",
        AcquireEvent::Validation => "validation",
        AcquireEvent::ZeroSize => "zero-size",
    }
}

/// Consecutive non-success acquire events and un-recovered surface recreations.
#[derive(Debug, Default)]
pub struct SurfaceState {
    streak: u32,
    recreations: u32,
    /// Earliest instant the next acquire may happen; set by every retry-class action so unrelated
    /// redraw/input events cannot pull the attempt forward. Cleared by success or `on_visible`.
    retry_not_before: Option<Instant>,
    /// Occluded or zero-size: never acquire until `on_visible`.
    suspended: bool,
}

/// What the host should do with a redraw request or due repaint deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Acquire the surface texture now.
    AcquireNow,
    /// Do not acquire; look again at this instant.
    WaitUntil(Instant),
    /// Nothing to do until an external event (visibility, resize, new deadline).
    Idle,
}

impl SurfaceState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consecutive non-success events since the last success.
    pub fn streak(&self) -> u32 {
        self.streak
    }

    /// Clear the failure streak; call on `WindowEvent::Occluded(false)` /
    /// `Resized`, since a visibility or geometry change invalidates the
    /// previous failure mode.
    pub fn on_visible(&mut self) {
        self.streak = 0;
        self.retry_not_before = None;
        self.suspended = false;
    }

    /// Gate for acquisition: `Err(remaining)` while a scheduled retry instant (or, with
    /// `Duration::MAX`, a suspension) has not passed. `now` is passed in; no clock is read here.
    pub fn may_acquire(&self, now: Instant) -> Result<(), Duration> {
        if self.suspended {
            return Err(Duration::MAX);
        }
        match self.retry_not_before {
            Some(at) if at > now => Err(at - now),
            _ => Ok(()),
        }
    }

    /// Pure host-scheduling decision. A redraw is due when requested (any source: input, egui,
    /// resize) or when `next_repaint` has arrived; a due redraw still only acquires if the gate
    /// allows, otherwise the host waits for the retry instant.
    pub fn schedule(
        &self,
        now: Instant,
        next_repaint: Option<Instant>,
        redraw_requested: bool,
    ) -> Schedule {
        let due = redraw_requested || next_repaint.is_some_and(|at| at <= now);
        if !due {
            return next_repaint.map_or(Schedule::Idle, Schedule::WaitUntil);
        }
        match self.may_acquire(now) {
            Ok(()) => Schedule::AcquireNow,
            Err(Duration::MAX) => Schedule::Idle,
            Err(remaining) => Schedule::WaitUntil(now.checked_add(remaining).unwrap_or(now)),
        }
    }

    /// Drive the policy for one acquire attempt made at `now`; retry-class actions arm the gate.
    pub fn on_event(&mut self, event: AcquireEvent, now: Instant) -> Action {
        let action = self.decide(event);
        self.arm(&action, now);
        action
    }

    fn arm(&mut self, action: &Action, now: Instant) {
        self.retry_not_before = match action {
            Action::Retry { after }
            | Action::Reconfigure { after }
            | Action::RecreateSurface { after } => Some(now.checked_add(*after).unwrap_or(now)),
            _ => None,
        };
        match action {
            Action::WaitForVisibility | Action::Suspend => self.suspended = true,
            Action::Present => self.suspended = false,
            _ => {}
        }
    }

    fn decide(&mut self, event: AcquireEvent) -> Action {
        match event {
            AcquireEvent::Presented | AcquireEvent::Suboptimal => {
                self.streak = 0;
                self.recreations = 0;
                Action::Present
            }
            AcquireEvent::Occluded => Action::WaitForVisibility,
            AcquireEvent::ZeroSize => Action::Suspend,
            AcquireEvent::Validation => Action::Fatal(SurfaceFailure::Validation),
            AcquireEvent::Timeout => self.counted(event, |after| Action::Retry { after }),
            AcquireEvent::Outdated => self.counted(event, |after| Action::Reconfigure { after }),
            AcquireEvent::Lost => {
                self.recreations += 1;
                if self.recreations > MAX_RECREATIONS {
                    return Action::Fatal(SurfaceFailure::DeviceLost);
                }
                self.counted(event, |after| Action::RecreateSurface { after })
            }
        }
    }

    /// Surface recreation itself failed. Counts toward the streak; if the
    /// streak is not yet exhausted, retry with the standard backoff, otherwise
    /// fail terminally with the recreation error message.
    pub fn on_recreate_failed(&mut self, msg: String, now: Instant) -> Action {
        self.streak += 1;
        let action = if self.streak >= MAX_STREAK {
            Action::Fatal(SurfaceFailure::RecreateFailed(msg))
        } else {
            Action::Retry {
                after: self.backoff(),
            }
        };
        self.arm(&action, now);
        action
    }

    /// Count one non-success event; when the streak is exhausted return
    /// `RetriesExhausted`, otherwise build the retry `Action` from the backoff.
    fn counted(&mut self, event: AcquireEvent, retry: impl FnOnce(Duration) -> Action) -> Action {
        self.streak += 1;
        if self.streak >= MAX_STREAK {
            Action::Fatal(SurfaceFailure::RetriesExhausted {
                last: label(event),
                attempts: self.streak,
            })
        } else {
            retry(self.backoff())
        }
    }

    /// `min(16 ms * 2^(streak-1), 1 s)`; streak is >= 1 at every call site.
    fn backoff(&self) -> Duration {
        let scaled = FIRST_RETRY
            .as_millis()
            .saturating_mul(1u128 << (self.streak - 1).min(31));
        Duration::from_millis(scaled.min(MAX_BACKOFF.as_millis()) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(m: u64) -> Duration {
        Duration::from_millis(m)
    }

    /// Fixed fake-clock origin; tests advance it with `+ ms(..)`.
    fn t0() -> Instant {
        static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *ORIGIN.get_or_init(Instant::now)
    }

    #[test]
    fn backoff_doubles_from_16ms_capped_at_1s_then_fatal_on_eighth() {
        let mut state = SurfaceState::new();
        let expected = [16u64, 32, 64, 128, 256, 512, 1000];
        for (index, want) in expected.iter().enumerate() {
            let action = state.on_event(AcquireEvent::Timeout, t0());
            assert_eq!(action, Action::Retry { after: ms(*want) }, "retry {index}");
            assert_eq!(state.streak(), index as u32 + 1);
        }
        assert_eq!(
            state.on_event(AcquireEvent::Timeout, t0()),
            Action::Fatal(SurfaceFailure::RetriesExhausted {
                last: "timeout",
                attempts: MAX_STREAK,
            })
        );
    }

    #[test]
    fn success_resets_streak_and_recreations() {
        let mut state = SurfaceState::new();
        state.on_event(AcquireEvent::Lost, t0());
        for _ in 0..4 {
            state.on_event(AcquireEvent::Timeout, t0());
        }
        assert_eq!(state.streak(), 5);
        assert_eq!(
            state.on_event(AcquireEvent::Presented, t0()),
            Action::Present,
            "present resets"
        );
        assert_eq!(state.streak(), 0);
        // recreations reset too: three fresh recreations must be allowed
        // again even though one was consumed before the present.
        for _ in 0..3 {
            assert!(
                matches!(
                    state.on_event(AcquireEvent::Lost, t0()),
                    Action::RecreateSurface { .. }
                ),
                "recreation budget was reset by the present"
            );
        }
        assert_eq!(
            state.on_event(AcquireEvent::Lost, t0()),
            Action::Fatal(SurfaceFailure::DeviceLost)
        );
        // Suboptimal counts as a success.
        let mut state = SurfaceState::new();
        state.on_event(AcquireEvent::Timeout, t0());
        assert_eq!(state.streak(), 1);
        assert_eq!(
            state.on_event(AcquireEvent::Suboptimal, t0()),
            Action::Present,
            "suboptimal resets"
        );
        assert_eq!(state.streak(), 0);
    }

    #[test]
    fn four_lost_without_success_is_device_lost() {
        let mut state = SurfaceState::new();
        for _ in 0..MAX_RECREATIONS {
            assert!(
                matches!(
                    state.on_event(AcquireEvent::Lost, t0()),
                    Action::RecreateSurface { .. }
                ),
                "each of the first {MAX_RECREATIONS} losses recreates with backoff"
            );
        }
        assert_eq!(
            state.on_event(AcquireEvent::Lost, t0()),
            Action::Fatal(SurfaceFailure::DeviceLost)
        );
    }

    #[test]
    fn lost_presented_lost_thrice_is_not_fatal() {
        let mut state = SurfaceState::new();
        assert!(matches!(
            state.on_event(AcquireEvent::Lost, t0()),
            Action::RecreateSurface { .. }
        ));
        assert_eq!(
            state.on_event(AcquireEvent::Presented, t0()),
            Action::Present
        );
        for _ in 0..3 {
            assert!(
                matches!(
                    state.on_event(AcquireEvent::Lost, t0()),
                    Action::RecreateSurface { .. }
                ),
                "recreation budget was reset by the intervening present"
            );
        }
    }

    #[test]
    fn occluded_does_not_count_toward_streak_and_visible_clears_it() {
        let mut state = SurfaceState::new();
        state.on_event(AcquireEvent::Timeout, t0());
        state.on_event(AcquireEvent::Timeout, t0());
        assert_eq!(state.streak(), 2);
        assert_eq!(
            state.on_event(AcquireEvent::Occluded, t0()),
            Action::WaitForVisibility
        );
        assert_eq!(state.streak(), 2, "occlusion is not a failure");
        state.on_visible();
        assert_eq!(state.streak(), 0);
        // Backoff restarts from the first retry after visibility returns.
        assert_eq!(
            state.on_event(AcquireEvent::Timeout, t0()),
            Action::Retry { after: ms(16) }
        );
    }

    #[test]
    fn zero_size_suspends_and_validation_is_fatal() {
        let mut state = SurfaceState::new();
        assert_eq!(
            state.on_event(AcquireEvent::ZeroSize, t0()),
            Action::Suspend
        );
        assert_eq!(state.streak(), 0, "zero size is not a failure");
        assert_eq!(
            state.on_event(AcquireEvent::Validation, t0()),
            Action::Fatal(SurfaceFailure::Validation)
        );
    }

    #[test]
    fn recreate_failure_retries_then_fails_with_message() {
        let mut state = SurfaceState::new();
        assert_eq!(
            state.on_recreate_failed("boom".into(), t0()),
            Action::Retry { after: ms(16) }
        );
        for _ in 2..MAX_STREAK {
            state.on_event(AcquireEvent::Timeout, t0());
        }
        assert_eq!(state.streak(), MAX_STREAK - 1);
        assert_eq!(
            state.on_recreate_failed("boom".into(), t0()),
            Action::Fatal(SurfaceFailure::RecreateFailed("boom".into()))
        );
    }

    #[test]
    fn redraw_storm_during_backoff_never_acquires_before_deadline() {
        let mut state = SurfaceState::new();
        let start = t0();
        // Fail until the backoff is 512 ms; the gate then arms at start + 512 ms.
        let mut action = state.on_event(AcquireEvent::Timeout, start);
        for _ in 0..5 {
            action = state.on_event(AcquireEvent::Timeout, start);
        }
        assert_eq!(action, Action::Retry { after: ms(512) });
        let deadline = start + ms(512);
        let mut acquisitions = 0;
        for tick in 0..512u64 {
            // A redraw request every 1 ms (input/egui/resize-free events).
            let now = start + ms(tick);
            match state.schedule(now, Some(deadline), true) {
                Schedule::AcquireNow => acquisitions += 1,
                Schedule::WaitUntil(at) => assert_eq!(at, deadline),
                Schedule::Idle => panic!("a pending retry must stay scheduled"),
            }
            assert!(state.may_acquire(now).is_err());
        }
        assert_eq!(acquisitions, 0, "no acquisition before the retry instant");
        assert_eq!(
            state.schedule(deadline, Some(deadline), false),
            Schedule::AcquireNow
        );
        assert_eq!(state.may_acquire(deadline), Ok(()));
    }

    #[test]
    fn every_retry_class_action_arms_the_gate() {
        let start = t0();
        for event in [
            AcquireEvent::Timeout,
            AcquireEvent::Outdated,
            AcquireEvent::Lost,
        ] {
            let mut state = SurfaceState::new();
            state.on_event(event, start);
            assert_eq!(state.may_acquire(start), Err(ms(16)), "{event:?}");
            assert_eq!(state.may_acquire(start + ms(10)), Err(ms(6)), "{event:?}");
            assert_eq!(state.may_acquire(start + ms(16)), Ok(()), "{event:?}");
        }
        let mut state = SurfaceState::new();
        state.on_recreate_failed("x".into(), start);
        assert_eq!(state.may_acquire(start), Err(ms(16)));
    }

    #[test]
    fn deadline_alone_wakes_without_a_request_and_future_deadline_waits() {
        let state = SurfaceState::new();
        let start = t0();
        let later = start + ms(100);
        assert_eq!(
            state.schedule(start, Some(later), false),
            Schedule::WaitUntil(later)
        );
        assert_eq!(
            state.schedule(later, Some(later), false),
            Schedule::AcquireNow
        );
        assert_eq!(state.schedule(start, None, false), Schedule::Idle);
    }

    #[test]
    fn visibility_lifts_the_gate_and_success_clears_it() {
        let mut state = SurfaceState::new();
        let start = t0();
        state.on_event(AcquireEvent::Timeout, start);
        assert!(state.may_acquire(start).is_err());
        state.on_visible();
        assert_eq!(state.may_acquire(start), Ok(()));
        state.on_event(AcquireEvent::Timeout, start);
        state.on_event(AcquireEvent::Presented, start + ms(16));
        assert_eq!(state.may_acquire(start + ms(16)), Ok(()));
    }

    #[test]
    fn occluded_and_zero_size_never_acquire_until_visible() {
        let start = t0();
        for event in [AcquireEvent::Occluded, AcquireEvent::ZeroSize] {
            let mut state = SurfaceState::new();
            state.on_event(event, start);
            for tick in [0u64, 1, 1000, 1_000_000] {
                let now = start + ms(tick);
                assert_eq!(
                    state.schedule(now, Some(now), true),
                    Schedule::Idle,
                    "{event:?}"
                );
                assert!(state.may_acquire(now).is_err());
            }
            state.on_visible();
            assert_eq!(state.schedule(start, None, true), Schedule::AcquireNow);
        }
    }

    #[test]
    fn fatal_does_not_leave_a_stale_gate() {
        let mut state = SurfaceState::new();
        let start = t0();
        for _ in 0..MAX_STREAK {
            state.on_event(AcquireEvent::Timeout, start);
        }
        assert_eq!(state.may_acquire(start), Ok(()));
    }
}
