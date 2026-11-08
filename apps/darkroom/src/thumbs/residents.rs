//! Texture residency (byte budget, provisional flags) and the bounded memo used by the store.

use std::collections::{HashMap, HashSet, VecDeque};

use super::ThumbKey;

/// Texture residency under a byte budget. Generic over the payload so tests need no egui.
pub struct Residents<T> {
    entries: HashMap<ThumbKey, Entry<T>>,
    bytes: u64,
}

struct Entry<T> {
    value: T,
    size: [u32; 2],
    bytes: u64,
    last_used: u64,
    /// Served instead of the edited tier (canonical/plain/generated) while the edited render is
    /// still owed.
    provisional: bool,
}

impl<T> Default for Residents<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
        }
    }
}

impl<T> Residents<T> {
    pub fn touch(&mut self, key: &ThumbKey, frame: u64) -> Option<(&T, [u32; 2])> {
        let entry = self.entries.get_mut(key)?;
        entry.last_used = frame;
        Some((&entry.value, entry.size))
    }

    #[cfg(test)]
    pub fn insert(&mut self, key: ThumbKey, value: T, size: [u32; 2], frame: u64) {
        self.insert_with(key, value, size, frame, false);
    }

    pub fn insert_with(
        &mut self,
        key: ThumbKey,
        value: T,
        size: [u32; 2],
        frame: u64,
        provisional: bool,
    ) {
        let bytes = 4 * u64::from(size[0]) * u64::from(size[1]);
        let entry = Entry {
            value,
            size,
            bytes,
            last_used: frame,
            provisional,
        };
        if let Some(old) = self.entries.insert(key, entry) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
    }

    /// Drop least-recently-requested entries NOT requested in `frame` until under `budget`.
    pub fn evict(&mut self, frame: u64, budget: u64) {
        while self.bytes > budget {
            let victim = self
                .entries
                .iter()
                .filter(|(_, entry)| entry.last_used < frame)
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone());
            let Some(entry) = victim.and_then(|key| self.entries.remove(&key)) else {
                break; // everything left is on screen
            };
            self.bytes -= entry.bytes;
        }
    }

    #[cfg(test)]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Any resident texture of the same image bytes under another edit version: shown while the
    /// current version loads, so an edit never flashes the cell back to a placeholder.
    pub fn touch_previous(&mut self, key: &ThumbKey, frame: u64) -> Option<(&T, [u32; 2])> {
        let previous = self
            .entries
            .iter()
            .filter(|(other, _)| other.image_id == key.image_id && other.hash == key.hash)
            .max_by_key(|(other, _)| other.edited_at)
            .map(|(other, _)| other.clone())?;
        self.touch(&previous, frame)
    }

    pub fn is_provisional(&self, key: &ThumbKey) -> bool {
        self.entries.get(key).is_some_and(|entry| entry.provisional)
    }

    /// Forget one key's texture (its bytes leave the budget).
    pub fn remove(&mut self, key: &ThumbKey) {
        if let Some(entry) = self.entries.remove(key) {
            self.bytes -= entry.bytes;
        }
    }

    /// Forget every provisional texture (a new catalog epoch re-asks, and so re-ensures, them).
    pub fn remove_provisional(&mut self) {
        let keys: Vec<ThumbKey> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.provisional)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            self.remove(&key);
        }
    }

    /// The newest settled (non-provisional) texture of the same image bytes under ANOTHER edit
    /// version: shown while `key`'s own edited render is owed, so a save never flashes the cell
    /// back to the unedited thumbnail.
    pub fn touch_settled_previous(&mut self, key: &ThumbKey, frame: u64) -> Option<(&T, [u32; 2])> {
        let previous = self
            .entries
            .iter()
            .filter(|(other, entry)| {
                *other != key
                    && !entry.provisional
                    && other.image_id == key.image_id
                    && other.hash == key.hash
            })
            .max_by_key(|(other, _)| other.edited_at)
            .map(|(other, _)| other.clone())?;
        self.touch(&previous, frame)
    }
}

/// Insertion-ordered key set that forgets its oldest member beyond `cap`.
pub struct BoundedKeys {
    set: HashSet<ThumbKey>,
    order: VecDeque<ThumbKey>,
    cap: usize,
}

impl BoundedKeys {
    pub fn new(cap: usize) -> Self {
        Self {
            set: HashSet::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    pub fn contains(&self, key: &ThumbKey) -> bool {
        self.set.contains(key)
    }

    pub fn insert(&mut self, key: ThumbKey) {
        if self.set.insert(key.clone()) {
            self.order.push_back(key);
        }
        while self.set.len() > self.cap {
            match self.order.pop_front() {
                Some(oldest) => self.set.remove(&oldest),
                None => break,
            };
        }
    }

    pub fn clear(&mut self) {
        self.set.clear();
        self.order.clear();
    }
}
