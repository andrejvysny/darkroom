//! Deferred-write bookkeeping: the failed-write backlog and its exponential backoff, kept apart
//! from the scheduling loop so each can be read on its own. Everything here touches only
//! `QueueState::deferred` (plus the full-rescan flag, to bound the backlog's memory).

use super::*;

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(60);

fn backoff(attempts: u32) -> Duration {
    let base = BACKOFF_BASE.as_millis() as u64;
    let ms = base.saturating_mul(2u64.saturating_pow(attempts.saturating_sub(1)));
    Duration::from_millis(ms).min(BACKOFF_CAP)
}

/// A failed write kept for a later attempt. The entry stays in `deferred` while the id is being
/// retried (in flight or waiting for `due`) so its attempt count survives until a success removes it.
pub(super) struct Deferred {
    pub(super) attempts: u32,
    pub(super) due: Instant,
    pub(super) last_error: Option<String>,
}

impl SidecarQueue {
    /// Park a failed id until `due`. `error` is kept for [`Self::pending_report`] so quit UI can say
    /// *why* the queue is not drained. The entry outlives the in-flight retry (removed only on
    /// success) so attempt counts never reset just because a retry was handed to the worker.
    pub(super) fn defer(&self, id: i64, error: Option<String>, now: Instant) {
        let mut st = self.lock();
        st.dirty.remove(&id);
        let attempts = st.deferred.get(&id).map_or(0, |d| d.attempts) + 1;
        if !st.deferred.contains_key(&id) && st.dirty.len() + st.deferred.len() >= MAX_PENDING {
            // Same memory bound as `mark_dirty_many`: an unbounded deferred set on a permanently
            // failing volume is worse than a rescan that will re-derive the same dirty ids.
            st.scan = Some(i64::MIN);
            st.scan_generation = st.scan_generation.wrapping_add(1);
            drop(st);
            self.inner.cv.notify_all();
            return;
        }
        st.deferred.insert(
            id,
            Deferred {
                attempts,
                due: now + backoff(attempts),
                last_error: error,
            },
        );
        drop(st);
        self.inner.cv.notify_all();
    }

    /// A `Written`/`Gone` id is finished: drop any backoff record it left behind.
    pub(super) fn settle(&self, ids: &[i64]) {
        if ids.is_empty() {
            return;
        }
        let mut st = self.lock();
        for &id in ids {
            st.deferred.remove(&id);
        }
        drop(st);
        self.inner.cv.notify_all();
    }

    /// Move every deferred id that is due (all of them, once, on the shutdown pass) into a batch.
    /// Entries stay in `deferred` while in flight: only `settle` (a successful write) removes them,
    /// so a failing retry keeps its attempt count and backs off further next time.
    pub(super) fn take_deferred(st: &mut QueueState, now: Instant) -> Option<Batch> {
        if st.shutdown && st.final_pass {
            return None;
        }
        let last_chance = st.shutdown;
        st.final_pass |= last_chance;
        let ids: Vec<i64> = st
            .deferred
            .iter()
            .filter(|(_, d)| last_chance || d.due <= now)
            .map(|(&id, _)| id)
            .collect();
        if ids.is_empty() {
            return None;
        }
        st.in_flight = ids.len();
        Some(Batch::Dirty(ids))
    }

    /// How long `next_batch` may sleep before the earliest backoff is due, or `None` to park
    /// indefinitely (nothing deferred).
    pub(super) fn earliest_due_timeout(st: &QueueState, now: Instant) -> Option<Duration> {
        st.deferred
            .values()
            .map(|d| d.due)
            .min()?
            .checked_duration_since(now)
    }
}
