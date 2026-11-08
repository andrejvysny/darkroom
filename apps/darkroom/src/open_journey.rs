//! Opt-in open-latency diagnostics (`DARKROOM_TRACE_OPEN=1`): how long a newly selected photo
//! takes from the selection command to each presentation milestone, plus the photo worker's
//! per-request stage timings. One stderr line per finished journey/request.
//!
//! The journey starts when the host dispatches `SelectImage` (one frame after the input event at
//! most), so it measures what the user waits for, not just the worker's share of it.

use core_model::identity::ImageId;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("DARKROOM_TRACE_OPEN").is_some_and(|v| v != "0"))
}

/// A presentation milestone of the photo being opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Milestone {
    /// A cached picture (thumbnail or preview) of the photo is on screen.
    Placeholder,
    /// The first live render (any quality tier) is on screen.
    Live,
    /// The first full-quality (`Settled`) render is on screen; ends the journey.
    Settled,
}

/// Elapsed time from selection to each milestone reached so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journey {
    pub image: ImageId,
    started: Instant,
    pub placeholder: Option<Duration>,
    pub live: Option<Duration>,
    pub settled: Option<Duration>,
}

impl Journey {
    fn line(&self) -> String {
        let ms =
            |d: Option<Duration>| d.map_or_else(|| "-".to_owned(), |d| d.as_millis().to_string());
        format!(
            "open/journey image={} placeholder_ms={} live_ms={} settled_ms={}",
            self.image.0,
            ms(self.placeholder),
            ms(self.live),
            ms(self.settled)
        )
    }
}

/// The journey in flight (at most one: a new selection abandons the previous one) and the last
/// finished one.
#[derive(Default)]
pub struct Journeys {
    current: Option<Journey>,
    last: Option<Journey>,
}

impl Journeys {
    pub fn begin(&mut self, image: ImageId, now: Instant) {
        self.current = Some(Journey {
            image,
            started: now,
            placeholder: None,
            live: None,
            settled: None,
        });
    }

    /// Record `milestone` for `image` (first occurrence only). A milestone of another photo
    /// than the one being opened is ignored.
    pub fn mark(&mut self, image: ImageId, milestone: Milestone, now: Instant) {
        let Some(journey) = self.current.as_mut().filter(|j| j.image == image) else {
            return;
        };
        let elapsed = now.saturating_duration_since(journey.started);
        let slot = match milestone {
            Milestone::Placeholder => &mut journey.placeholder,
            Milestone::Live => &mut journey.live,
            Milestone::Settled => &mut journey.settled,
        };
        slot.get_or_insert(elapsed);
        if milestone == Milestone::Settled {
            // A settled frame is also live.
            journey.live.get_or_insert(elapsed);
            let done = self.current.take();
            if let Some(done) = done {
                if enabled() {
                    eprintln!("{}", done.line());
                }
                self.last = Some(done);
            }
        }
    }

    #[cfg(test)]
    pub fn last(&self) -> Option<&Journey> {
        self.last.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journey_records_each_milestone_once_and_ends_on_settled() {
        let t0 = Instant::now();
        let mut journeys = Journeys::default();
        journeys.begin(ImageId(7), t0);
        journeys.mark(
            ImageId(7),
            Milestone::Placeholder,
            t0 + Duration::from_millis(5),
        );
        journeys.mark(
            ImageId(7),
            Milestone::Placeholder,
            t0 + Duration::from_millis(9),
        );
        journeys.mark(ImageId(7), Milestone::Live, t0 + Duration::from_millis(300));
        journeys.mark(
            ImageId(7),
            Milestone::Settled,
            t0 + Duration::from_millis(400),
        );
        let last = journeys.last().expect("finished");
        assert_eq!(last.placeholder, Some(Duration::from_millis(5)));
        assert_eq!(last.live, Some(Duration::from_millis(300)));
        assert_eq!(last.settled, Some(Duration::from_millis(400)));
    }

    #[test]
    fn milestones_of_another_photo_are_ignored_and_settled_implies_live() {
        let t0 = Instant::now();
        let mut journeys = Journeys::default();
        journeys.begin(ImageId(1), t0);
        journeys.mark(
            ImageId(2),
            Milestone::Settled,
            t0 + Duration::from_millis(10),
        );
        assert!(journeys.last().is_none());
        journeys.mark(
            ImageId(1),
            Milestone::Settled,
            t0 + Duration::from_millis(20),
        );
        let last = journeys.last().expect("finished");
        assert_eq!(last.live, Some(Duration::from_millis(20)));
    }
}
