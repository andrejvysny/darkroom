//! Pure autosave coordinator: per-image debounced saves with revision-exact acknowledgement.
//!
//! Port of `src/lib/developPersistence.ts`. No threads and no clocks: every time-dependent
//! entry point takes `now_ms: u64` from the caller, so the policy is deterministic and testable.
//! The host owns the timers (`next_deadline` tells it when to call `due` again) and executes each
//! [`SaveRequest`] as a catalog commit of exactly that revision, then reports back via
//! [`Autosave::completed`].
//!
//! Guarantees (legacy parity):
//! - per-image pending state: an edit to B never touches A's pending save;
//! - latest-wins: intermediate revisions collapse into the newest;
//! - serialized per image: at most one save in flight, and an older completion can never
//!   regress a newer acknowledgement;
//! - failures stay dirty and retry on a paced backoff, becoming visible once the backoff
//!   steps are exhausted.

use std::collections::HashMap;
use std::sync::Arc;

use core_model::identity::{EditRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;

/// Trailing debounce after the user stops moving a control.
pub const IDLE_MS: u64 = 250;
/// Hard ceiling between saves while the user keeps editing the same image.
pub const MAX_WAIT_MS: u64 = 1000;
/// Backoff after the 1st/2nd/3rd failed save; afterwards retries stay at the last step.
pub const RETRY_BACKOFF_MS: [u64; 3] = [500, 1500, 4000];

/// Banner shown once the backoff steps are exhausted (legacy `onError` message).
const VISIBLE_ERROR: &str = "Changes haven't been saved. Darkroom will keep trying.";

/// One save the host must execute (catalog commit of exactly this revision).
#[derive(Clone, Debug)]
pub struct SaveRequest {
    /// Image whose catalog edit is committed.
    pub image: ImageId,
    /// Exact revision the catalog must accept; acknowledgement is revision-exact.
    pub revision: EditRevision,
    /// Parameters captured at dispatch time (latest-wins snapshot).
    pub params: Arc<DevelopParams>,
    /// Process version the params were produced by, carried to the catalog.
    pub process_version: ProcessVersion,
    /// `record` calls since the last successful save (behavioural log input).
    pub touch_count: u32,
}

/// Revisions captured at a moment in time; flushed when every listed image has saved >= it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FlushTarget(pub Vec<(ImageId, EditRevision)>);

/// Observable pending state for one image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSave {
    /// The image.
    pub image: ImageId,
    /// Newest revision recorded for it.
    pub target: EditRevision,
    /// Highest revision acknowledged by a successful save, if any.
    pub saved: Option<EditRevision>,
    /// Consecutive failed save attempts.
    pub failures: u32,
    /// Error message of the most recent failure.
    pub last_error: Option<String>,
}

/// The save the host is currently executing for this image, if any.
#[derive(Clone, Debug)]
struct InFlight {
    revision: EditRevision,
    /// `touch_count` captured at dispatch; subtracted from the entry on success (legacy
    /// `touchCount - touches` — touches arriving during the flight belong to the next save).
    touches: u32,
}

#[derive(Clone, Debug)]
struct Entry {
    latest: Arc<DevelopParams>,
    process_version: ProcessVersion,
    /// Newest recorded revision; saves target this (latest-wins).
    latest_revision: EditRevision,
    /// Highest acknowledged revision; the entry is dirty while it trails `latest_revision`.
    saved: Option<EditRevision>,
    touch_count: u32,
    /// When the image went clean -> dirty; the `MAX_WAIT_MS` anchor.
    dirty_since: u64,
    /// Last `record` time; the idle debounce anchor.
    last_record_at: u64,
    /// Set after a failed save; dispatch is paced to this instant (see `record`).
    retry_at: Option<u64>,
    inflight: Option<InFlight>,
    failures: u32,
    last_error: Option<String>,
}

impl Entry {
    fn is_dirty(&self) -> bool {
        self.saved.is_none_or(|s| s.0 < self.latest_revision.0)
    }

    /// When this entry may next be dispatched. A scheduled failure retry dominates: retries
    /// are paced so a failing backend is not hammered by an ongoing gesture, and latest-wins
    /// guarantees the newest params ride the retry anyway.
    fn deadline(&self) -> u64 {
        match self.retry_at {
            Some(t) => t,
            None => (self.last_record_at + IDLE_MS).min(self.dirty_since + MAX_WAIT_MS),
        }
    }
}

/// Backoff delay after the `failures`-th consecutive failure (>= 1); paced forever at the
/// last step once the table is exhausted.
fn backoff_for(failures: u32) -> u64 {
    let idx = usize::try_from(failures.clamp(1, RETRY_BACKOFF_MS.len() as u32) - 1)
        .unwrap_or(RETRY_BACKOFF_MS.len() - 1);
    RETRY_BACKOFF_MS[idx]
}

/// Per-image autosave state machine; pure data, driven by `now_ms` from the host.
#[derive(Debug, Default)]
pub struct Autosave {
    entries: HashMap<ImageId, Entry>,
    /// Set by `stop_admission` (shutdown): no new records, existing work still drains.
    admission_closed: bool,
}

impl Autosave {
    /// Empty coordinator accepting records.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new dirty revision. Ignored (returns false) when `revision` is not newer than
    /// what is already recorded for the image, or after `stop_admission`.
    ///
    /// A record during a failure backoff deliberately does NOT shorten the backoff (legacy
    /// behaviour: failure retries are paced so a broken catalog does not get hammered while the
    /// user drags a slider; latest-wins means the retry still carries the newest params).
    pub fn record(
        &mut self,
        image: ImageId,
        revision: EditRevision,
        params: Arc<DevelopParams>,
        pv: ProcessVersion,
        now_ms: u64,
    ) -> bool {
        if self.admission_closed {
            return false;
        }
        if self
            .entries
            .get(&image)
            .is_some_and(|e| revision.0 <= e.latest_revision.0)
        {
            return false;
        }
        let e = self.entries.entry(image).or_insert_with(|| Entry {
            latest: params.clone(),
            process_version: pv,
            latest_revision: revision,
            saved: None,
            touch_count: 0,
            dirty_since: now_ms,
            last_record_at: now_ms,
            retry_at: None,
            inflight: None,
            failures: 0,
            last_error: None,
        });
        if !e.is_dirty() {
            // Clean -> dirty while an older save is still in flight: re-anchor the MAX_WAIT
            // window for the new dirty episode (legacy `queue`).
            e.dirty_since = now_ms;
        }
        e.latest = params;
        e.process_version = pv;
        e.latest_revision = revision;
        e.touch_count += 1;
        e.last_record_at = now_ms;
        // `failures`/`retry_at`/`last_error` are deliberately untouched: continuing to edit
        // must not reset the backoff nor hide the banner.
        true
    }

    /// Saves that are due now (at most one per image, never for an image with a save in
    /// flight). Marks them in flight.
    pub fn due(&mut self, now_ms: u64) -> Vec<SaveRequest> {
        let ready: Vec<ImageId> = self
            .entries
            .iter()
            .filter(|(_, e)| e.inflight.is_none() && e.is_dirty() && e.deadline() <= now_ms)
            .map(|(id, _)| *id)
            .collect();
        self.dispatch(&ready)
    }

    /// Like `due` but ignores debounce/backoff timers (flush / quit). Still one in flight per
    /// image.
    pub fn due_now(&mut self) -> Vec<SaveRequest> {
        let ready: Vec<ImageId> = self
            .entries
            .iter()
            .filter(|(_, e)| e.inflight.is_none() && e.is_dirty())
            .map(|(id, _)| *id)
            .collect();
        self.dispatch(&ready)
    }

    /// Earliest time `due` could return something (for the host's timer), None if idle.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries
            .values()
            .filter(|e| e.inflight.is_none() && e.is_dirty())
            .map(Entry::deadline)
            .min()
    }

    /// Completion of a request. `Ok` acknowledges exactly `request.revision`:
    /// saved = max(saved, revision). `Err(msg)` counts a failure and schedules the backoff
    /// retry of the LATEST revision. Unknown/duplicate completions are ignored safely.
    pub fn completed(
        &mut self,
        image: ImageId,
        revision: EditRevision,
        result: Result<(), String>,
        now_ms: u64,
    ) {
        let Some(e) = self.entries.get_mut(&image) else {
            return; // unknown image (already saved-and-pruned, or discarded)
        };
        let matches_flight = e.inflight.as_ref().is_some_and(|f| f.revision == revision);
        match result {
            Ok(()) => {
                if matches_flight {
                    if let Some(f) = e.inflight.take() {
                        e.touch_count = e.touch_count.saturating_sub(f.touches);
                    }
                    e.failures = 0;
                    e.last_error = None;
                    e.retry_at = None;
                }
                // A save that executed did commit exactly this revision, so the ack always
                // applies — even out of order or as a duplicate; the max keeps `saved`
                // monotonic so an older completion can never regress a newer acknowledgement.
                let ack = match e.saved {
                    Some(s) if s.0 >= revision.0 => s,
                    _ => revision,
                };
                e.saved = Some(ack);
            }
            Err(msg) => {
                if !matches_flight {
                    return; // stray/duplicate failure: nothing is waiting on it
                }
                e.inflight = None;
                e.failures += 1;
                e.retry_at = Some(now_ms.saturating_add(backoff_for(e.failures)));
                e.last_error = Some(msg);
            }
        }
        if !e.is_dirty() && e.inflight.is_none() {
            // Clean and idle — drop the entry so the map does not grow with the library.
            self.entries.remove(&image);
        }
    }

    /// Capture the current newest revision of every dirty image.
    pub fn flush_target(&self) -> FlushTarget {
        let mut target: Vec<(ImageId, EditRevision)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_dirty())
            .map(|(id, e)| (*id, e.latest_revision))
            .collect();
        target.sort_by_key(|(id, _)| id.0);
        FlushTarget(target)
    }

    /// True when every (image, rev) in `target` has saved >= rev.
    ///
    /// An absent entry counts as satisfied: its save was acknowledged and the entry pruned, or
    /// the user explicitly discarded the image — discard is an explicit user decision and must
    /// not pin a flush/quit barrier forever.
    pub fn is_flushed(&self, target: &FlushTarget) -> bool {
        target.0.iter().all(|(image, rev)| {
            self.entries
                .get(image)
                .is_none_or(|e| e.saved.is_some_and(|s| s.0 >= rev.0))
        })
    }

    /// Pending state of every tracked image, sorted by image id for determinism.
    pub fn pending(&self) -> Vec<PendingSave> {
        let mut pending: Vec<PendingSave> = self
            .entries
            .iter()
            .map(|(id, e)| PendingSave {
                image: *id,
                target: e.latest_revision,
                saved: e.saved,
                failures: e.failures,
                last_error: e.last_error.clone(),
            })
            .collect();
        pending.sort_by_key(|p| p.image.0);
        pending
    }

    /// Visible error: Some when any image has exhausted the backoff table.
    pub fn visible_error(&self) -> Option<String> {
        self.entries
            .values()
            .any(|e| e.failures > RETRY_BACKOFF_MS.len() as u32)
            .then(|| VISIBLE_ERROR.to_string())
    }

    /// Stop accepting new records (shutdown); in-flight + already-recorded work continues.
    pub fn stop_admission(&mut self) {
        self.admission_closed = true;
    }

    /// Explicit user choice to discard unsaved edits for these images (quit "discard").
    ///
    /// In-flight saves are simply forgotten; a late completion for a discarded image finds no
    /// entry and is ignored (if the catalog commit did land, that is a bonus, not a regression).
    pub fn discard(&mut self, images: &[ImageId]) {
        for image in images {
            self.entries.remove(image);
        }
    }

    /// True when nothing is dirty and nothing is in flight.
    pub fn is_idle(&self) -> bool {
        self.entries
            .values()
            .all(|e| !e.is_dirty() && e.inflight.is_none())
    }

    fn dispatch(&mut self, images: &[ImageId]) -> Vec<SaveRequest> {
        let mut ids = images.to_vec();
        ids.sort_by_key(|id| id.0);
        let mut requests = Vec::new();
        for id in ids {
            let Some(e) = self.entries.get_mut(&id) else {
                continue;
            };
            if e.inflight.is_some() || !e.is_dirty() {
                continue;
            }
            e.retry_at = None;
            e.inflight = Some(InFlight {
                revision: e.latest_revision,
                touches: e.touch_count,
            });
            requests.push(SaveRequest {
                image: id,
                revision: e.latest_revision,
                params: e.latest.clone(),
                process_version: e.process_version,
                touch_count: e.touch_count,
            });
        }
        requests
    }
}
