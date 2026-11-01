//! Bounded surface-acquire state machine.
//!
//! Deliberately free of `wgpu`/`winit` types: `window.rs` classifies
//! `wgpu::CurrentSurfaceTexture` variants into [`AcquireEvent`] and this module
//! decides what to do about it, so the retry/backoff policy is unit-testable
//! without a GPU or a window.

use std::time::Duration;

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
    }

    /// Drive the policy for one acquire attempt.
    pub fn on_event(&mut self, event: AcquireEvent) -> Action {
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
    pub fn on_recreate_failed(&mut self, msg: String) -> Action {
        self.streak += 1;
        if self.streak >= MAX_STREAK {
            Action::Fatal(SurfaceFailure::RecreateFailed(msg))
        } else {
            Action::Retry {
                after: self.backoff(),
            }
        }
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

    #[test]
    fn backoff_doubles_from_16ms_capped_at_1s_then_fatal_on_eighth() {
        let mut state = SurfaceState::new();
        let expected = [16u64, 32, 64, 128, 256, 512, 1000];
        for (index, want) in expected.iter().enumerate() {
            let action = state.on_event(AcquireEvent::Timeout);
            assert_eq!(action, Action::Retry { after: ms(*want) }, "retry {index}");
            assert_eq!(state.streak(), index as u32 + 1);
        }
        assert_eq!(
            state.on_event(AcquireEvent::Timeout),
            Action::Fatal(SurfaceFailure::RetriesExhausted {
                last: "timeout",
                attempts: MAX_STREAK,
            })
        );
    }

    #[test]
    fn success_resets_streak_and_recreations() {
        let mut state = SurfaceState::new();
        state.on_event(AcquireEvent::Lost);
        for _ in 0..4 {
            state.on_event(AcquireEvent::Timeout);
        }
        assert_eq!(state.streak(), 5);
        assert_eq!(
            state.on_event(AcquireEvent::Presented),
            Action::Present,
            "present resets"
        );
        assert_eq!(state.streak(), 0);
        // recreations reset too: three fresh recreations must be allowed
        // again even though one was consumed before the present.
        for _ in 0..3 {
            assert!(
                matches!(
                    state.on_event(AcquireEvent::Lost),
                    Action::RecreateSurface { .. }
                ),
                "recreation budget was reset by the present"
            );
        }
        assert_eq!(
            state.on_event(AcquireEvent::Lost),
            Action::Fatal(SurfaceFailure::DeviceLost)
        );
        // Suboptimal counts as a success.
        let mut state = SurfaceState::new();
        state.on_event(AcquireEvent::Timeout);
        assert_eq!(state.streak(), 1);
        assert_eq!(
            state.on_event(AcquireEvent::Suboptimal),
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
                    state.on_event(AcquireEvent::Lost),
                    Action::RecreateSurface { .. }
                ),
                "each of the first {MAX_RECREATIONS} losses recreates with backoff"
            );
        }
        assert_eq!(
            state.on_event(AcquireEvent::Lost),
            Action::Fatal(SurfaceFailure::DeviceLost)
        );
    }

    #[test]
    fn lost_presented_lost_thrice_is_not_fatal() {
        let mut state = SurfaceState::new();
        assert!(matches!(
            state.on_event(AcquireEvent::Lost),
            Action::RecreateSurface { .. }
        ));
        assert_eq!(state.on_event(AcquireEvent::Presented), Action::Present);
        for _ in 0..3 {
            assert!(
                matches!(
                    state.on_event(AcquireEvent::Lost),
                    Action::RecreateSurface { .. }
                ),
                "recreation budget was reset by the intervening present"
            );
        }
    }

    #[test]
    fn occluded_does_not_count_toward_streak_and_visible_clears_it() {
        let mut state = SurfaceState::new();
        state.on_event(AcquireEvent::Timeout);
        state.on_event(AcquireEvent::Timeout);
        assert_eq!(state.streak(), 2);
        assert_eq!(
            state.on_event(AcquireEvent::Occluded),
            Action::WaitForVisibility
        );
        assert_eq!(state.streak(), 2, "occlusion is not a failure");
        state.on_visible();
        assert_eq!(state.streak(), 0);
        // Backoff restarts from the first retry after visibility returns.
        assert_eq!(
            state.on_event(AcquireEvent::Timeout),
            Action::Retry { after: ms(16) }
        );
    }

    #[test]
    fn zero_size_suspends_and_validation_is_fatal() {
        let mut state = SurfaceState::new();
        assert_eq!(state.on_event(AcquireEvent::ZeroSize), Action::Suspend);
        assert_eq!(state.streak(), 0, "zero size is not a failure");
        assert_eq!(
            state.on_event(AcquireEvent::Validation),
            Action::Fatal(SurfaceFailure::Validation)
        );
    }

    #[test]
    fn recreate_failure_retries_then_fails_with_message() {
        let mut state = SurfaceState::new();
        assert_eq!(
            state.on_recreate_failed("boom".into()),
            Action::Retry { after: ms(16) }
        );
        for _ in 2..MAX_STREAK {
            state.on_event(AcquireEvent::Timeout);
        }
        assert_eq!(state.streak(), MAX_STREAK - 1);
        assert_eq!(
            state.on_recreate_failed("boom".into()),
            Action::Fatal(SurfaceFailure::RecreateFailed("boom".into()))
        );
    }
}
