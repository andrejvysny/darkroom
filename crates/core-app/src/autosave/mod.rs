//! Pure autosave coordinator: per-document debounced saves with seq-exact acknowledgement.
//!
//! Port of `src/lib/developPersistence.ts`. No threads and no clocks: every time-dependent
//! entry point takes `now_ms: u64` from the caller, so the policy is deterministic and testable.
//! The host owns the timers (`next_deadline` tells it when to call `due` again) and executes each
//! [`SaveRequest`] as a catalog commit of exactly that save, then reports back via
//! [`Autosave::completed`].
//!
//! Identity. Editor revisions restart at 1 in every `EditorSession`, so they can never order saves
//! across a close/reopen of the same image. State is therefore keyed by [`SaveKey`] (catalog
//! epoch + image + source content hash, which survives sessions) and ordered by the coordinator's
//! own never-reset [`SaveSeq`]. The editor-local `(session, revision)` pair rides along as an
//! [`EditOrigin`] purely so the session can tell whether an acknowledgement still concerns the
//! live document.
//!
//! Guarantees (legacy parity, now across editor reopen):
//! - per-key pending state: an edit to B never touches A's pending save;
//! - latest-wins: intermediate seqs collapse into the newest;
//! - serialized per key: at most one save in flight, even when the editor was closed and
//!   reopened in between, and an older completion can never regress a newer acknowledgement;
//! - failures stay dirty and retry on a paced backoff, becoming visible once the backoff
//!   steps are exhausted.

use std::collections::HashMap;
use std::sync::Arc;

use core_model::identity::{CatalogEpoch, CatalogRevision, ImageId, ProcessVersion};
use core_model::DevelopParams;

/// Trailing debounce after the user stops moving a control.
pub const IDLE_MS: u64 = 250;
/// Hard ceiling between saves while the user keeps editing the same image.
pub const MAX_WAIT_MS: u64 = 1000;
/// Backoff after the 1st/2nd/3rd failed save; afterwards retries stay at the last step.
pub const RETRY_BACKOFF_MS: [u64; 3] = [500, 1500, 4000];

/// Banner shown once the backoff steps are exhausted (legacy `onError` message).
const VISIBLE_ERROR: &str = "Changes haven't been saved. Darkroom will keep trying.";

mod types;
pub use types::*;

/// The save the host is currently executing for this key, if any.
#[derive(Clone, Debug)]
struct InFlight {
    seq: SaveSeq,
    origin: EditOrigin,
    /// `touch_count` captured at dispatch; subtracted from the entry on success (legacy
    /// `touchCount - touches` — touches arriving during the flight belong to the next save).
    touches: u32,
}

#[derive(Clone, Debug)]
struct Entry {
    latest: Arc<DevelopParams>,
    process_version: ProcessVersion,
    /// Newest assigned seq; saves target this (latest-wins).
    latest_seq: SaveSeq,
    latest_origin: EditOrigin,
    /// Highest acknowledged seq; the entry is dirty while it trails `latest_seq`.
    saved: Option<SaveSeq>,
    touch_count: u32,
    /// When the key went clean -> dirty; the `MAX_WAIT_MS` anchor.
    dirty_since: u64,
    /// Last `record` time; the idle debounce anchor.
    last_record_at: u64,
    /// Set after a failed save; dispatch is paced to this instant (see `record`).
    retry_at: Option<u64>,
    inflight: Option<InFlight>,
    failures: u32,
    last_error: Option<String>,
    /// The source changed on disk: dirty forever, never dispatched, only discardable.
    blocked: bool,
}

impl Entry {
    fn dispatchable(&self) -> bool {
        self.inflight.is_none() && self.is_dirty() && !self.blocked
    }

    fn is_dirty(&self) -> bool {
        self.saved.is_none_or(|s| s < self.latest_seq)
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

/// Per-key autosave state machine; pure data, driven by `now_ms` from the host.
#[derive(Debug, Default)]
pub struct Autosave {
    entries: HashMap<SaveKey, Entry>,
    /// Highest acknowledged seq per key; survives entry pruning so an absent entry proves
    /// "flushed" only when an ack actually happened (never because it was dropped).
    acked: HashMap<SaveKey, SaveSeq>,
    next_seq: u64,
    /// Set by `stop_admission` (shutdown): no new seqs, existing work still drains.
    admission_closed: bool,
}

impl Autosave {
    /// Empty coordinator accepting records.
    pub fn new() -> Self {
        Self::default()
    }

    /// True when new edits are accepted.
    pub fn admits(&self) -> bool {
        !self.admission_closed
    }

    /// Record the editor's newest state for `key`.
    ///
    /// Params that differ from the entry's latest get a fresh [`SaveSeq`] (returns true); equal
    /// params assign nothing but re-bind the latest seq to `origin` (a reopened session takes over
    /// the pending state it restored). Returns false for no new seq, and always after
    /// `stop_admission`.
    ///
    /// A record during a failure backoff deliberately does NOT shorten the backoff (legacy
    /// behaviour: failure retries are paced so a broken catalog does not get hammered while the
    /// user drags a slider; latest-wins means the retry still carries the newest params).
    pub fn record(
        &mut self,
        key: SaveKey,
        origin: EditOrigin,
        params: Arc<DevelopParams>,
        pv: ProcessVersion,
        now_ms: u64,
    ) -> bool {
        if let Some(e) = self.entries.get_mut(&key) {
            if *e.latest == *params && e.process_version == pv {
                e.latest_origin = origin;
                return false;
            }
        }
        if self.admission_closed {
            return false;
        }
        let seq = SaveSeq(self.next_seq + 1);
        self.next_seq = seq.0;
        let e = self.entries.entry(key).or_insert_with(|| Entry {
            latest: params.clone(),
            process_version: pv,
            latest_seq: seq,
            latest_origin: origin,
            saved: None,
            touch_count: 0,
            dirty_since: now_ms,
            last_record_at: now_ms,
            retry_at: None,
            inflight: None,
            failures: 0,
            last_error: None,
            blocked: false,
        });
        if !e.is_dirty() {
            // Clean -> dirty while an older save is still in flight: re-anchor the MAX_WAIT
            // window for the new dirty episode (legacy `queue`).
            e.dirty_since = now_ms;
        }
        e.latest = params;
        e.process_version = pv;
        e.latest_seq = seq;
        e.latest_origin = origin;
        e.touch_count += 1;
        e.last_record_at = now_ms;
        // `failures`/`retry_at`/`last_error` are deliberately untouched: continuing to edit
        // must not reset the backoff nor hide the banner.
        true
    }

    /// Saves that are due now (at most one per key, never for a key with a save in flight).
    /// Marks them in flight.
    pub fn due(&mut self, now_ms: u64) -> Vec<SaveRequest> {
        let ready: Vec<SaveKey> = self
            .entries
            .iter()
            .filter(|(_, e)| e.dispatchable() && e.deadline() <= now_ms)
            .map(|(k, _)| k.clone())
            .collect();
        self.dispatch(ready)
    }

    /// Like `due` but ignores debounce/backoff timers (flush / quit). Still one in flight per key.
    pub fn due_now(&mut self) -> Vec<SaveRequest> {
        let ready: Vec<SaveKey> = self
            .entries
            .iter()
            .filter(|(_, e)| e.dispatchable())
            .map(|(k, _)| k.clone())
            .collect();
        self.dispatch(ready)
    }

    /// Earliest time `due` could return something (for the host's timer), None if idle.
    pub fn next_deadline(&self) -> Option<u64> {
        self.entries
            .values()
            .filter(|e| e.dispatchable())
            .map(Entry::deadline)
            .min()
    }

    /// Completion of a request, as [`Autosave::completed_outcome`] but flattened to the ack.
    pub fn completed(
        &mut self,
        key: &SaveKey,
        seq: SaveSeq,
        result: Result<CatalogRevision, SaveFailure>,
        now_ms: u64,
    ) -> Option<Ack> {
        match self.completed_outcome(key, seq, result, now_ms) {
            Completed::Acked(ack) => Some(ack),
            Completed::Stale | Completed::Failed => None,
        }
    }

    /// Completion of a request. `Ok(rev)` acknowledges exactly `seq`; `Err` counts a failure and
    /// schedules the backoff retry of the LATEST seq, except [`SaveFailure::SourceChanged`], which
    /// blocks the entry. Completions matching no in-flight save change nothing ([`Completed::Stale`]).
    pub fn completed_outcome(
        &mut self,
        key: &SaveKey,
        seq: SaveSeq,
        result: Result<CatalogRevision, SaveFailure>,
        now_ms: u64,
    ) -> Completed {
        let Some(e) = self.entries.get_mut(key) else {
            return Completed::Stale;
        };
        if e.inflight.as_ref().is_none_or(|f| f.seq != seq) {
            return Completed::Stale;
        }
        let Some(flight) = e.inflight.take() else {
            return Completed::Stale;
        };
        let outcome = match result {
            Ok(catalog_revision) => {
                e.touch_count = e.touch_count.saturating_sub(flight.touches);
                e.failures = 0;
                e.last_error = None;
                e.retry_at = None;
                // Monotonic: an acknowledgement can never move `saved` backwards.
                e.saved = Some(e.saved.map_or(seq, |s| s.max(seq)));
                let acked = self.acked.entry(key.clone()).or_insert(seq);
                *acked = (*acked).max(seq);
                let origin = if seq == e.latest_seq {
                    e.latest_origin
                } else {
                    flight.origin
                };
                Completed::Acked(Ack {
                    seq,
                    origin,
                    catalog_revision,
                })
            }
            Err(failure) => {
                e.failures += 1;
                match failure {
                    SaveFailure::Retryable(msg) => {
                        e.retry_at = Some(now_ms.saturating_add(backoff_for(e.failures)));
                        e.last_error = Some(msg);
                    }
                    SaveFailure::SourceChanged(msg) => {
                        e.blocked = true;
                        e.retry_at = None;
                        e.last_error = Some(msg);
                    }
                }
                Completed::Failed
            }
        };
        if !e.is_dirty() && e.inflight.is_none() {
            // Clean and idle — drop the entry so the map does not grow with the library.
            self.entries.remove(key);
        }
        outcome
    }

    /// Capture the current newest seq of every dirty key.
    pub fn flush_target(&self) -> FlushTarget {
        let mut target: Vec<(SaveKey, SaveSeq)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_dirty())
            .map(|(k, e)| (k.clone(), e.latest_seq))
            .collect();
        target.sort_by_key(|(k, seq)| (k.image_id.0, *seq));
        FlushTarget(target)
    }

    /// True when every (key, seq) in `target` has been acknowledged at >= seq.
    ///
    /// A pruned entry proves this only through the recorded ack: dropping a key (discard, epoch
    /// change) never counts as flushed.
    pub fn is_flushed(&self, target: &FlushTarget) -> bool {
        target
            .0
            .iter()
            .all(|(key, seq)| self.is_flushed_one(key, *seq))
    }

    /// True when `key` has an acknowledged save at >= `seq`.
    pub fn is_flushed_one(&self, key: &SaveKey, seq: SaveSeq) -> bool {
        let saved = self.entries.get(key).and_then(|e| e.saved);
        saved.max(self.acked.get(key).copied()) >= Some(seq)
    }

    /// Pending state of every tracked key, sorted by image id for determinism.
    pub fn pending(&self) -> Vec<PendingSave> {
        let mut pending: Vec<PendingSave> = self
            .entries
            .iter()
            .map(|(k, e)| PendingSave {
                key: k.clone(),
                target: e.latest_seq,
                saved: e.saved,
                failures: e.failures,
                last_error: e.last_error.clone(),
            })
            .collect();
        pending.sort_by_key(|p| (p.key.image_id.0, p.target));
        pending
    }

    /// Unsaved state of `key` (dirty or still in flight): the newest params and their process
    /// version, so a reopened editor can restore them instead of the older stored document.
    pub fn latest_unsaved(&self, key: &SaveKey) -> Option<(Arc<DevelopParams>, ProcessVersion)> {
        self.entries
            .get(key)
            .filter(|e| e.is_dirty() || e.inflight.is_some())
            .map(|e| (e.latest.clone(), e.process_version))
    }

    /// True while a save for `key` is executing.
    pub fn is_saving(&self, key: &SaveKey) -> bool {
        self.entries.get(key).is_some_and(|e| e.inflight.is_some())
    }

    /// Failure state for `key`: `(consecutive failures, last error)`, if it has failed.
    pub fn failure(&self, key: &SaveKey) -> Option<(u32, String)> {
        self.entries
            .get(key)
            .and_then(|e| e.last_error.clone().map(|m| (e.failures, m)))
    }

    /// Visible error: Some when any key has exhausted the backoff table.
    pub fn visible_error(&self) -> Option<String> {
        self.entries
            .values()
            .any(|e| e.failures > RETRY_BACKOFF_MS.len() as u32)
            .then(|| VISIBLE_ERROR.to_string())
    }

    /// Stop admitting new seqs (shutdown); in-flight + already-recorded work continues.
    pub fn stop_admission(&mut self) {
        self.admission_closed = true;
    }

    /// Re-open admission after a cancelled shutdown.
    pub fn resume_admission(&mut self) {
        self.admission_closed = false;
    }

    /// Explicit user choice to discard unsaved edits for these keys. An in-flight save cannot be
    /// recalled, so such keys are kept and returned; everything else is removed.
    pub fn discard(&mut self, keys: &[SaveKey]) -> Vec<SaveKey> {
        let mut refused = Vec::new();
        for key in keys {
            if self.entries.get(key).is_some_and(|e| e.inflight.is_some()) {
                refused.push(key.clone());
            } else {
                self.entries.remove(key);
            }
        }
        refused
    }

    /// Keys tracked for `image` in `epoch`.
    pub fn keys_for_image(&self, epoch: CatalogEpoch, image: ImageId) -> Vec<SaveKey> {
        self.entries
            .keys()
            .filter(|k| k.catalog_epoch == epoch && k.image_id == image)
            .cloned()
            .collect()
    }

    /// Keys whose source changed on disk (dirty, undispatchable until discarded).
    pub fn blocked_keys(&self) -> Vec<SaveKey> {
        let mut keys: Vec<SaveKey> = self
            .entries
            .iter()
            .filter(|(_, e)| e.blocked)
            .map(|(k, _)| k.clone())
            .collect();
        keys.sort_by_key(|k| k.image_id.0);
        keys
    }

    /// True when `key` is blocked by a changed source.
    pub fn is_blocked(&self, key: &SaveKey) -> bool {
        self.entries.get(key).is_some_and(|e| e.blocked)
    }

    /// Forget every key of another catalog epoch. Refuses (returning the offenders, dropping
    /// nothing) while any of them is dirty or in flight: unsaved work is never silently dropped.
    pub fn retain_epoch(&mut self, epoch: CatalogEpoch) -> Result<(), Vec<SaveKey>> {
        let unsaved: Vec<SaveKey> = self
            .entries
            .iter()
            .filter(|(k, e)| k.catalog_epoch != epoch && (e.is_dirty() || e.inflight.is_some()))
            .map(|(k, _)| k.clone())
            .collect();
        if !unsaved.is_empty() {
            return Err(unsaved);
        }
        self.entries.retain(|k, _| k.catalog_epoch == epoch);
        self.acked.retain(|k, _| k.catalog_epoch == epoch);
        Ok(())
    }

    /// Number of tracked keys (upper bound on one dispatch round).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no key is tracked.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True when nothing is dirty and nothing is in flight.
    pub fn is_idle(&self) -> bool {
        self.entries
            .values()
            .all(|e| !e.is_dirty() && e.inflight.is_none())
    }

    fn dispatch(&mut self, mut keys: Vec<SaveKey>) -> Vec<SaveRequest> {
        keys.sort_by_key(|k| (k.image_id.0, k.catalog_epoch.0));
        let mut requests = Vec::new();
        for key in keys {
            let Some(e) = self.entries.get_mut(&key) else {
                continue;
            };
            if e.inflight.is_some() || !e.is_dirty() {
                continue;
            }
            e.retry_at = None;
            e.inflight = Some(InFlight {
                seq: e.latest_seq,
                origin: e.latest_origin,
                touches: e.touch_count,
            });
            requests.push(SaveRequest {
                seq: e.latest_seq,
                origin: e.latest_origin,
                params: e.latest.clone(),
                process_version: e.process_version,
                touch_count: e.touch_count,
                key,
            });
        }
        requests
    }
}
