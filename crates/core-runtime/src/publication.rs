//! Identity fence for publishing work derived from catalog sources.
use core_model::identity::{CatalogEpoch, ImageId};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceKey {
    pub hash: String,
    pub path: String,
}

#[derive(Clone, Debug)]
pub struct PublicationToken {
    image_id: ImageId,
    key: SourceKey,
    epoch: u64,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stale;

struct Entry {
    key: Option<SourceKey>,
    revision: u64,
}

#[derive(Default)]
struct State {
    epoch: u64,
    sequence: u64,
    sources: HashMap<ImageId, Entry>,
}

/// Serializes only final publication and source identity mutations.
#[derive(Default)]
pub struct PublicationGate {
    state: Mutex<State>,
}

pub struct SourceMutationGuard<'a> {
    state: std::sync::MutexGuard<'a, State>,
}
fn retire_state(state: &mut State, id: ImageId) {
    state.sequence = state
        .sequence
        .checked_add(1)
        .expect("publication sequence exhausted");
    let entry = state.sources.entry(id).or_insert(Entry {
        key: None,
        revision: 0,
    });
    entry.revision = entry
        .revision
        .checked_add(1)
        .expect("source revision exhausted");
    entry.key = None;
}

fn replace_source_state(state: &mut State, id: ImageId, key: SourceKey) {
    state.sequence = state
        .sequence
        .checked_add(1)
        .expect("publication sequence exhausted");
    let entry = state.sources.entry(id).or_insert(Entry {
        key: None,
        revision: 0,
    });
    entry.revision = entry
        .revision
        .checked_add(1)
        .expect("source revision exhausted");
    entry.key = Some(key);
}

impl SourceMutationGuard<'_> {
    pub fn retire(&mut self, id: ImageId) {
        retire_state(&mut self.state, id);
    }

    pub fn replace_source(&mut self, id: ImageId, key: SourceKey) {
        replace_source_state(&mut self.state, id, key);
    }

    pub fn epoch(&self) -> CatalogEpoch {
        CatalogEpoch(self.state.epoch)
    }

    pub fn invalidate_all(&mut self) {
        self.state.epoch = self
            .state
            .epoch
            .checked_add(1)
            .expect("catalog epoch exhausted");
        self.state.sequence = self
            .state
            .sequence
            .checked_add(1)
            .expect("publication sequence exhausted");
        self.state.sources.clear();
    }
}

impl PublicationGate {
    pub fn source_guard(&self) -> SourceMutationGuard<'_> {
        SourceMutationGuard {
            state: self.state.lock().expect("publication gate poisoned"),
        }
    }

    pub fn epoch(&self) -> CatalogEpoch {
        CatalogEpoch(self.state.lock().expect("publication gate poisoned").epoch)
    }

    /// Changes on every source mutation, including same-key replacement.
    pub fn sequence(&self) -> u64 {
        self.state
            .lock()
            .expect("publication gate poisoned")
            .sequence
    }

    pub fn capture(&self, id: ImageId, key: SourceKey) -> Result<PublicationToken, Stale> {
        self.capture_inner(id, key, None)
    }

    pub fn capture_at(
        &self,
        id: ImageId,
        key: SourceKey,
        sequence: u64,
    ) -> Result<PublicationToken, Stale> {
        self.capture_inner(id, key, Some(sequence))
    }

    fn capture_inner(
        &self,
        id: ImageId,
        key: SourceKey,
        sequence: Option<u64>,
    ) -> Result<PublicationToken, Stale> {
        let mut state = self.state.lock().expect("publication gate poisoned");
        if sequence.is_some_and(|expected| expected != state.sequence) {
            return Err(Stale);
        }
        let epoch = state.epoch;
        let entry = state.sources.entry(id).or_insert_with(|| Entry {
            key: Some(key.clone()),
            revision: 0,
        });
        if entry.key.as_ref() != Some(&key) {
            return Err(Stale);
        }
        Ok(PublicationToken {
            image_id: id,
            key,
            epoch,
            revision: entry.revision,
        })
    }

    pub fn with_current<T>(
        &self,
        token: &PublicationToken,
        f: impl FnOnce() -> T,
    ) -> Result<T, Stale> {
        let state = self.state.lock().expect("publication gate poisoned");
        let current = state.sources.get(&token.image_id);
        if state.epoch != token.epoch
            || !current
                .is_some_and(|e| e.revision == token.revision && e.key.as_ref() == Some(&token.key))
        {
            return Err(Stale);
        }
        Ok(f())
    }

    /// Lock order: publication commit gate before any catalog lock.
    pub fn mutation_guard(&self) -> impl Drop + '_ {
        self.state.lock().expect("publication gate poisoned")
    }

    /// Validate `tokens`, then hand out the source-mutation guard and hold the gate across the
    /// caller's catalog commit — the same validation and lock order as [`Self::with_tokens`]
    /// (gate before catalog), but the caller keeps the guard, so a row inserted by that commit can
    /// register its identity via [`SourceMutationGuard::replace_source`] without releasing the gate
    /// (a released gate would let a concurrent retire/reset slip between commit and registration).
    pub fn tokens_guard(
        &self,
        tokens: &[PublicationToken],
    ) -> Result<SourceMutationGuard<'_>, Stale> {
        let state = self.state.lock().expect("publication gate poisoned");
        if tokens.iter().any(|token| {
            state.epoch != token.epoch
                || !state.sources.get(&token.image_id).is_some_and(|entry| {
                    entry.revision == token.revision && entry.key.as_ref() == Some(&token.key)
                })
        }) {
            return Err(Stale);
        }
        Ok(SourceMutationGuard { state })
    }

    pub fn with_tokens<T>(
        &self,
        tokens: &[PublicationToken],
        f: impl FnOnce() -> T,
    ) -> Result<T, Stale> {
        let _guard = self.tokens_guard(tokens)?;
        Ok(f())
    }
    pub fn with_sequence<T>(&self, sequence: u64, f: impl FnOnce() -> T) -> Result<T, Stale> {
        let state = self.state.lock().expect("publication gate poisoned");
        if state.sequence != sequence {
            return Err(Stale);
        }
        Ok(f())
    }
    /// A source-set mutation retires all earlier identities before the catalog changes.
    pub fn invalidate_all_guard(&self) -> impl Drop + '_ {
        let mut state = self.state.lock().expect("publication gate poisoned");
        state.epoch = state.epoch.checked_add(1).expect("catalog epoch exhausted");
        state.sequence = state
            .sequence
            .checked_add(1)
            .expect("publication sequence exhausted");
        state.sources.clear();
        state
    }

    /// Serialize a catalog mutation with final publication without changing source identity.
    pub fn with_mutation<T>(&self, f: impl FnOnce() -> T) -> T {
        let _state = self.state.lock().expect("publication gate poisoned");
        f()
    }

    /// Hold across the catalog commit (lock order: gate before catalog). Use the
    /// guard to record source identity changes made by that commit.
    pub fn commit_guard(&self, epoch: CatalogEpoch) -> Result<SourceMutationGuard<'_>, Stale> {
        let state = self.state.lock().expect("publication gate poisoned");
        if state.epoch != epoch.0 {
            return Err(Stale);
        }
        Ok(SourceMutationGuard { state })
    }

    /// Runs `f` while holding an epoch-fenced commit guard; `Err(Stale)` if the
    /// epoch is no longer current by the time the gate is acquired.
    pub fn with_epoch<T>(&self, epoch: CatalogEpoch, f: impl FnOnce() -> T) -> Result<T, Stale> {
        let _guard = self.commit_guard(epoch)?;
        Ok(f())
    }

    pub fn retire(&self, id: ImageId) {
        self.retire_with(id, || ());
    }
    pub fn retire_with<T>(&self, id: ImageId, f: impl FnOnce() -> T) -> T {
        let mut state = self.state.lock().expect("publication gate poisoned");
        retire_state(&mut state, id);
        f()
    }

    pub fn replace_source(&self, id: ImageId, key: SourceKey) {
        self.replace_source_with(id, key, || ());
    }
    pub fn register_source(&self, id: ImageId, key: SourceKey) {
        self.replace_source(id, key);
    }
    pub fn replace_source_with<T>(&self, id: ImageId, key: SourceKey, f: impl FnOnce() -> T) -> T {
        let mut state = self.state.lock().expect("publication gate poisoned");
        replace_source_state(&mut state, id, key);
        f()
    }

    pub fn invalidate_all(&self) {
        self.invalidate_all_with(|| ());
    }
    pub fn invalidate_all_with<T>(&self, f: impl FnOnce() -> T) -> T {
        let mut state = self.state.lock().expect("publication gate poisoned");
        state.epoch = state.epoch.checked_add(1).expect("catalog epoch exhausted");
        state.sequence = state
            .sequence
            .checked_add(1)
            .expect("publication sequence exhausted");
        state.sources.clear();
        f()
    }
}
