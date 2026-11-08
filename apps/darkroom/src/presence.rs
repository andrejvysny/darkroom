//! Background "does the original file still exist" checks behind `ui_egui::ThumbSource::presence`.
//!
//! The UI thread only reads the cache; one worker thread stats the rows that were asked for.
//! [`PresenceStore::invalidate`] keeps the last answer on screen (no badge flicker) but marks it
//! stale so the next ask re-checks it.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::ErrorKind;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use ui_egui::Presence;

/// Cached answers; past this, entries not asked for recently are dropped.
const CACHE_CAP: usize = 4096;
/// An entry not asked for in this many requests is evictable.
const RECENT: u64 = 2048;
/// Rows waiting for a stat; later asks re-queue them.
const PENDING_CAP: usize = 1024;

type Key = (i64, String);

struct Entry {
    value: Presence,
    stale: bool,
    asked: u64,
}

#[derive(Default)]
struct Inner {
    cache: HashMap<Key, Entry>,
    inflight: HashSet<Key>,
    pending: VecDeque<Key>,
    tick: u64,
    /// Bumped by `invalidate`; an answer computed under an older one stays stale.
    generation: u64,
    stop: bool,
}

type Shared = Arc<(Mutex<Inner>, Condvar)>;

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct PresenceStore {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl PresenceStore {
    /// `wake` redraws the window when an answer changed.
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Result<Self, String> {
        let shared = Shared::default();
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("presence-worker".to_owned())
            .spawn(move || worker_loop(&worker, &*wake))
            .map_err(|error| format!("could not start presence worker: {error}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Cached answer; [`Presence::Unknown`] until the worker has checked. Never touches the disk.
    pub fn ask(&self, image_id: i64, path: &str) -> Presence {
        let (inner, cvar) = &*self.shared;
        let mut guard = lock(inner);
        guard.tick += 1;
        let tick = guard.tick;
        let key = (image_id, path.to_owned());
        let (value, needs_check) = match guard.cache.get_mut(&key) {
            Some(entry) => {
                entry.asked = tick;
                (entry.value, entry.stale)
            }
            None => (Presence::Unknown, true),
        };
        if needs_check && guard.pending.len() < PENDING_CAP && guard.inflight.insert(key.clone()) {
            guard.pending.push_back(key);
            cvar.notify_one();
        }
        value
    }

    /// Re-check every cached answer the next time it is asked for.
    pub fn invalidate(&self) {
        let mut guard = lock(&self.shared.0);
        guard.generation += 1;
        for entry in guard.cache.values_mut() {
            entry.stale = true;
        }
    }

    /// Stop and join the worker. Idempotent.
    pub fn shutdown(&mut self) -> Result<(), String> {
        lock(&self.shared.0).stop = true;
        self.shared.1.notify_all();
        match self.thread.take().map(JoinHandle::join) {
            Some(Err(_)) => Err("presence worker thread panicked".to_owned()),
            _ => Ok(()),
        }
    }
}

impl Drop for PresenceStore {
    fn drop(&mut self) {
        if let Err(error) = self.shutdown() {
            eprintln!("{error}");
        }
    }
}

/// Only a definite "not found" counts as missing: a permission error or a flaky mount must not
/// paint every photo as lost.
fn check(path: &str) -> Presence {
    match std::fs::metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Presence::Missing,
        _ => Presence::Present,
    }
}

fn worker_loop(shared: &(Mutex<Inner>, Condvar), wake: &(dyn Fn() + Send + Sync)) {
    let (inner, cvar) = shared;
    loop {
        let (key, generation) = {
            let mut guard = lock(inner);
            loop {
                if guard.stop {
                    return;
                }
                if let Some(key) = guard.pending.pop_front() {
                    break (key, guard.generation);
                }
                guard = cvar.wait(guard).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let value = check(&key.1);
        let changed = record(&mut lock(inner), key, value, generation);
        if changed {
            wake();
        }
    }
}

/// Stores one answer; true when it differs from what the UI last saw.
fn record(guard: &mut Inner, key: Key, value: Presence, generation: u64) -> bool {
    guard.inflight.remove(&key);
    let fresh = generation == guard.generation;
    let asked = guard.tick;
    let changed = match guard.cache.get_mut(&key) {
        Some(entry) => {
            let changed = entry.value != value;
            entry.value = value;
            entry.stale = !fresh;
            changed
        }
        None => {
            guard.cache.insert(
                key,
                Entry {
                    value,
                    stale: !fresh,
                    asked,
                },
            );
            true
        }
    };
    if guard.cache.len() > CACHE_CAP {
        let floor = guard.tick.saturating_sub(RECENT);
        guard.cache.retain(|_, entry| entry.asked >= floor);
    }
    changed
}

/// Message for a photo whose source cannot be opened because the file is gone. `None` for every
/// other failure, which keeps its raw text.
pub fn missing_source_message(path: &std::path::Path, error: &std::io::Error) -> Option<String> {
    (error.kind() == ErrorKind::NotFound).then(|| {
        format!(
            "{MISSING_SOURCE}\n{}\nReconnect the drive or restore the file, then Retry.",
            path.display()
        )
    })
}

/// First line of [`missing_source_message`]; the stage message needs no file-name suffix.
pub const MISSING_SOURCE: &str = "The original file is missing.";

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn store() -> (PresenceStore, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        (PresenceStore::new(wake).unwrap(), wakes)
    }

    fn wait_for(store: &PresenceStore, id: i64, path: &str, want: Presence) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if store.ask(id, path) == want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn present_then_missing_after_invalidate() {
        let dir = std::env::temp_dir().join(format!("darkroom-presence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.cr3");
        std::fs::write(&file, b"x").unwrap();
        let path = file.to_str().unwrap();
        let (store, wakes) = store();
        assert_eq!(
            store.ask(1, path),
            Presence::Unknown,
            "first ask never blocks"
        );
        assert!(wait_for(&store, 1, path, Presence::Present));
        assert!(wakes.load(Ordering::SeqCst) >= 1);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            store.ask(1, path),
            Presence::Present,
            "cached until invalidated"
        );
        store.invalidate();
        assert!(wait_for(&store, 1, path, Presence::Missing));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn absent_file_is_missing() {
        let (store, _) = store();
        let path = std::env::temp_dir().join("darkroom-presence-never-existed.cr3");
        let path = path.to_str().unwrap();
        assert_eq!(store.ask(7, path), Presence::Unknown);
        assert!(wait_for(&store, 7, path, Presence::Missing));
    }

    #[test]
    fn not_found_maps_to_the_missing_message() {
        let path = std::path::Path::new("/Volumes/Gone/IMG_1.CR3");
        let gone = std::io::Error::from(ErrorKind::NotFound);
        let message = missing_source_message(path, &gone).unwrap();
        assert_eq!(
            message,
            "The original file is missing.\n/Volumes/Gone/IMG_1.CR3\n\
             Reconnect the drive or restore the file, then Retry."
        );
        let denied = std::io::Error::from(ErrorKind::PermissionDenied);
        assert!(missing_source_message(path, &denied).is_none());
    }
}
