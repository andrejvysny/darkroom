//! Neighbour prefetch: a CPU decode thread plus a one-image handoff to the GPU worker.
//!
//! The thread decodes the ±1 neighbours of the selected photo with the FULL `develop_linear`
//! (the same pixels a render needs), one at a time, and parks the result in a bounded slot. The
//! GPU worker uploads it only when it is otherwise idle and memory admission allows (see
//! `PreparedCache::admit_speculative`); a render for a parked source takes it instead of decoding
//! again. A new list replaces the old one and cancels the in-flight decode at its next stage.
//!
//! Memory: the thread waits for a free slot BEFORE it starts decoding, and the slot stays
//! "busy" while the worker holds a taken image, so at most one decoded `LinearImage` exists.
//!
//! Lock order: the thread never takes the worker's inbox lock while holding this lock; the
//! worker takes this lock only while holding the inbox lock (in `next_job`).

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use core_raw::LinearImage;

use super::inbox::SlotPair;
use super::prepared::SourceKey;
use super::{PhotoWorker, WarmRequest};

/// A decoded neighbour on its way to the GPU worker.
pub(crate) type Decoded = (SourceKey, LinearImage);

#[derive(Default)]
struct State {
    /// Newest list not yet picked up by the thread.
    list: Option<Vec<WarmRequest>>,
    /// Bumped by every `submit`; a decode of an older one is cancelled.
    generation: u64,
    slot: Option<Decoded>,
    /// The worker holds an image taken from the slot; the slot counts as occupied until release.
    busy: bool,
    stop: bool,
}

pub(crate) struct Prefetch {
    state: Mutex<State>,
    cvar: Condvar,
    /// Sources the worker already has (cached or pinned), refreshed after every job.
    known: Mutex<HashSet<SourceKey>>,
    /// The worker's inbox, notified when an image lands (it sleeps on that condvar).
    inbox: Arc<SlotPair>,
}

/// Cancellation view handed to a decode: true once a newer list or shutdown arrived.
pub(crate) struct Cancel<'a> {
    prefetch: &'a Prefetch,
    generation: u64,
}

impl Cancel<'_> {
    pub(crate) fn is_cancelled(&self) -> bool {
        let state = self.prefetch.state();
        state.stop || state.generation != self.generation
    }
}

/// Decode one neighbour; `None` = unreadable or cancelled. Production: [`decode_full`].
pub(crate) type Decode = dyn Fn(&WarmRequest, &Cancel<'_>) -> Option<LinearImage> + Send;

impl Prefetch {
    pub(crate) fn new(inbox: Arc<SlotPair>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            cvar: Condvar::new(),
            known: Mutex::new(HashSet::new()),
            inbox,
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Latest-wins: replaces the unstarted list and cancels the decode of the previous one.
    pub(crate) fn submit(&self, list: Vec<WarmRequest>) {
        let mut state = self.state();
        state.list = Some(list);
        state.generation += 1;
        drop(state);
        self.cvar.notify_all();
    }

    pub(crate) fn stop(&self) {
        self.state().stop = true;
        self.cvar.notify_all();
    }

    pub(crate) fn set_known(&self, keys: HashSet<SourceKey>) {
        *self.known.lock().unwrap_or_else(PoisonError::into_inner) = keys;
    }

    fn is_known(&self, key: &SourceKey) -> bool {
        self.known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    /// An image is parked and the worker may upload it (called under the inbox lock).
    pub(crate) fn take(&self) -> Option<Decoded> {
        let mut state = self.state();
        let decoded = state.slot.take()?;
        state.busy = true;
        Some(decoded)
    }

    /// Promotion: a render/warm wants `key` and it is parked — decode no second time.
    pub(crate) fn take_matching(&self, key: &SourceKey) -> Option<LinearImage> {
        let mut state = self.state();
        if state.slot.as_ref().is_none_or(|(k, _)| k != key) {
            return None;
        }
        state.busy = true;
        state.slot.take().map(|(_, image)| image)
    }

    /// The worker is done with a taken image (any job end; idempotent): the thread may go on.
    pub(crate) fn release(&self) {
        let mut state = self.state();
        if state.busy {
            state.busy = false;
            drop(state);
            self.cvar.notify_all();
        }
    }

    #[cfg(test)]
    pub(crate) fn parked(&self) -> Option<SourceKey> {
        self.state().slot.as_ref().map(|(k, _)| k.clone())
    }

    fn next_list(&self) -> Option<(u64, Vec<WarmRequest>)> {
        let mut state = self.state();
        loop {
            if state.stop {
                return None;
            }
            if let Some(list) = state.list.take() {
                return Some((state.generation, list));
            }
            state = self
                .cvar
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Block until the slot is free. `false` = stop or a newer list (the caller abandons its list).
    fn wait_free(&self, generation: u64) -> bool {
        let mut state = self.state();
        while (state.slot.is_some() || state.busy) && !state.stop && state.generation == generation
        {
            state = self
                .cvar
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        !state.stop && state.generation == generation
    }

    /// Park a finished decode (dropped when cancelled meanwhile) and wake the idle worker.
    fn deposit(&self, generation: u64, decoded: Decoded) {
        let mut state = self.state();
        if state.stop || state.generation != generation || self.is_known(&decoded.0) {
            return;
        }
        state.slot = Some(decoded);
        drop(state);
        // Taking the inbox lock orders this after a worker that already saw an empty slot and is
        // about to sleep, so the wakeup cannot be lost.
        drop(self.inbox.0.lock().unwrap_or_else(PoisonError::into_inner));
        self.inbox.1.notify_all();
    }
}

impl PhotoWorker {
    /// Spawn the "photo-prefetch" decode thread with `decode` (tests inject a stub).
    pub(super) fn start_prefetch(&mut self, decode: Box<Decode>) -> Result<(), String> {
        let prefetch = Arc::clone(&self.prefetch);
        let thread = std::thread::Builder::new()
            .name("photo-prefetch".into())
            .spawn(move || run(&prefetch, &*decode))
            .map_err(|e| format!("could not start photo prefetch thread: {e}"))?;
        self.prefetch_thread = Some(thread);
        Ok(())
    }

    /// Decode these neighbours on the prefetch thread (latest wins; a new list cancels the old
    /// one between stages). They are uploaded only when the worker is idle and memory allows.
    pub fn prefetch(&self, neighbours: Vec<WarmRequest>) {
        self.prefetch.submit(neighbours);
    }
}

/// The thread body: lists in, at most one parked image out.
pub(crate) fn run(prefetch: &Prefetch, decode: &Decode) {
    while let Some((generation, list)) = prefetch.next_list() {
        for request in &list {
            let key = request.source_key();
            if prefetch.is_known(&key) {
                continue;
            }
            // Wait BEFORE decoding so a full slot never costs a second resident image.
            if !prefetch.wait_free(generation) {
                break;
            }
            let cancel = Cancel {
                prefetch,
                generation,
            };
            if let Some(image) = decode(request, &cancel) {
                prefetch.deposit(generation, (key, image));
            }
        }
    }
}

/// Production decode with a cancel check before the read, before the develop and after it.
pub(crate) fn decode_full(request: &WarmRequest, cancel: &Cancel<'_>) -> Option<LinearImage> {
    if cancel.is_cancelled() {
        return None;
    }
    let raw = core_raw::source_from_path(&request.path).ok()?;
    if cancel.is_cancelled() {
        return None;
    }
    let linear = core_raw::develop_linear(&raw).ok()?;
    (!cancel.is_cancelled()).then_some(linear)
}

#[cfg(test)]
#[path = "prefetch_tests.rs"]
mod tests;
