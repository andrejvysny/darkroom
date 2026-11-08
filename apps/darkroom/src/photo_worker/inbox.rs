//! The worker's inbox (latest-wins render + warm-up slots) and the supersession checks a running
//! job makes against it.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use super::prefetch::Prefetch;
use super::prepared::SourceKey;
use super::{Job, PhotoRequest, WarmRequest};

/// The worker's inbox. `request` (a render) always runs before `warm` (an early decode).
pub(crate) struct Slot {
    pub(crate) request: Option<PhotoRequest>,
    pub(crate) warm: Option<WarmRequest>,
    pub(crate) stop: bool,
    /// Bumped by the pool's return signal; a pool wait sleeps until it moves.
    pub(crate) pool_returns: u64,
}

pub(crate) type SlotPair = (Mutex<Slot>, Condvar);

/// What a running job may ask about newer work; all checks read the inbox under its lock.
pub(crate) struct Ctl<'a> {
    pub(crate) slot: &'a SlotPair,
}

impl Ctl<'_> {
    fn inbox(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn other_source(slot: &Slot, source: &SourceKey) -> bool {
        slot.stop
            || slot
                .request
                .as_ref()
                .is_some_and(|r| r.source_key() != *source)
            || slot
                .warm
                .as_ref()
                .is_some_and(|w| w.source_key() != *source)
    }

    /// Stop, or newer work wants a DIFFERENT photo: decoding/uploading `source` is wasted.
    pub(crate) fn source_superseded(&self, source: &SourceKey) -> bool {
        Self::other_source(&self.inbox(), source)
    }

    /// [`Self::source_superseded`], or any newer render is queued (latest-wins for output).
    pub(crate) fn output_superseded(&self, source: &SourceKey) -> bool {
        let slot = self.inbox();
        Self::other_source(&slot, source) || slot.request.is_some()
    }

    /// Stop, or any job is queued.
    pub(crate) fn has_pending(&self) -> bool {
        let slot = self.inbox();
        slot.stop || slot.request.is_some() || slot.warm.is_some()
    }

    pub(crate) fn pool_returns(&self) -> u64 {
        self.inbox().pool_returns
    }

    /// Sleep until a pooled target returned after `seen`, the output is superseded, or `timeout`.
    pub(crate) fn wait_pool(&self, seen: u64, source: &SourceKey, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut slot = self.inbox();
        while slot.pool_returns == seen
            && !Self::other_source(&slot, source)
            && slot.request.is_none()
        {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            slot = self
                .slot
                .1
                .wait_timeout(slot, deadline - now)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// Block until there is a job: a render first, then a warm-up, and only when neither is queued
/// the upload of a parked prefetch image. `None` = stop.
pub(crate) fn next_job(slot: &SlotPair, prefetch: Option<&Prefetch>) -> Option<Job> {
    let (lock, cvar) = slot;
    let mut guard = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        if guard.stop {
            return None;
        }
        if let Some(request) = guard.request.take() {
            return Some(Job::Render(request));
        }
        if let Some(warm) = guard.warm.take() {
            return Some(Job::Warm(warm));
        }
        if let Some(decoded) = prefetch.and_then(Prefetch::take) {
            return Some(Job::Upload(Box::new(decoded)));
        }
        guard = cvar
            .wait(guard)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
}
