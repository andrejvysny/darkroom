//! The scheduling state of the preview service: a two-tier id queue plus the paged backfill scan.
//!
//! Ported from the legacy `thumb_queue`: `front` (visible / just-opened) always runs before `back`
//! (bulk), the library is walked in `id > cursor` pages so it is never materialised, and the whole
//! thing is capped at [`MAX_PENDING`]. Every id is re-derivable from the catalog, so this state is
//! disposable and a poisoned lock is recovered, never propagated.

use std::collections::{HashSet, VecDeque};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// Ids waiting at once; beyond it the backfill pauses (the cursor stays) and a priority request
/// evicts the oldest bulk id (a later scan pass picks it up again).
pub const MAX_PENDING: usize = 4096;
/// Ids per backfill page.
pub const PAGE_SIZE: usize = 256;

#[derive(Default)]
pub(super) struct State {
    front: VecDeque<i64>,
    back: VecDeque<i64>,
    /// Membership set so an id is never queued twice.
    queued: HashSet<i64>,
    /// Next page starts after this id; `None` once the pass is complete.
    scan: Option<i64>,
    /// Bumped whenever the scan restarts, so a page read for the old pass is discarded.
    scan_generation: u64,
    paused: bool,
    stop: bool,
}

pub(super) enum Next {
    Render(i64),
    Refill { cursor: i64, generation: u64 },
    Stop,
}

pub(super) struct Queue {
    state: Mutex<State>,
    cv: Condvar,
}

impl Queue {
    /// A fresh queue whose first backfill pass is already scheduled.
    pub(super) fn new() -> Self {
        let state = State {
            scan: Some(i64::MIN),
            ..State::default()
        };
        Self {
            state: Mutex::new(state),
            cv: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queue `ids` ahead of the bulk work; the newest call runs first, ids within a call in order.
    pub(super) fn prioritize(&self, ids: &[i64]) {
        let mut st = self.lock();
        if st.stop {
            return;
        }
        for &id in ids.iter().rev() {
            st.back.retain(|&x| x != id);
            st.front.retain(|&x| x != id);
            if !st.queued.contains(&id) && st.queued.len() >= MAX_PENDING {
                if let Some(evicted) = st.back.pop_back().or_else(|| st.front.pop_back()) {
                    st.queued.remove(&evicted);
                    st.restart_scan();
                }
            }
            st.queued.insert(id);
            st.front.push_front(id);
        }
        drop(st);
        self.cv.notify_all();
    }

    pub(super) fn enqueue_back(&self, ids: &[i64]) {
        let mut st = self.lock();
        if st.stop {
            return;
        }
        for &id in ids {
            if st.queued.contains(&id) {
                continue;
            }
            if st.queued.len() >= MAX_PENDING {
                st.restart_scan();
                break;
            }
            st.queued.insert(id);
            st.back.push_back(id);
        }
        drop(st);
        self.cv.notify_all();
    }

    /// Put a render that was refused for now behind the bulk work, unless it is queued again.
    pub(super) fn requeue_back(&self, id: i64) {
        self.enqueue_back(&[id]);
    }

    /// A page for the pass named by `generation`; stale pages are dropped. A short page ends the
    /// pass; a page that does not fit leaves the cursor at the last id that did.
    pub(super) fn refill(&self, generation: u64, ids: &[i64]) {
        let mut st = self.lock();
        if st.scan_generation != generation {
            return;
        }
        st.scan = if ids.len() < PAGE_SIZE {
            None
        } else {
            ids.last().copied()
        };
        for &id in ids {
            if st.queued.contains(&id) {
                continue;
            }
            if st.queued.len() >= MAX_PENDING {
                st.scan = Some(id.saturating_sub(1));
                break;
            }
            st.queued.insert(id);
            st.back.push_back(id);
        }
        drop(st);
        self.cv.notify_all();
    }

    /// Forget everything queued and walk the library again (the open catalog was replaced).
    pub(super) fn reset(&self) {
        let mut st = self.lock();
        st.front.clear();
        st.back.clear();
        st.queued.clear();
        st.restart_scan();
        drop(st);
        self.cv.notify_all();
    }

    pub(super) fn set_paused(&self, paused: bool) {
        let mut st = self.lock();
        if st.paused != paused {
            st.paused = paused;
            drop(st);
            self.cv.notify_all();
        }
    }

    #[cfg(test)]
    pub(super) fn pending_ids(&self) -> Vec<i64> {
        let st = self.lock();
        st.front.iter().chain(st.back.iter()).copied().collect()
    }

    pub(super) fn stop(&self) {
        self.lock().stop = true;
        self.cv.notify_all();
    }

    /// Block until there is work and the service is not paused. The pause is checked before every
    /// job, so a pause lets the job in hand finish but starts no new one.
    pub(super) fn next(&self) -> Next {
        let mut st = self.lock();
        loop {
            if st.stop {
                return Next::Stop;
            }
            if !st.paused {
                if let Some(id) = st.front.pop_front().or_else(|| st.back.pop_front()) {
                    st.queued.remove(&id);
                    return Next::Render(id);
                }
                if let Some(cursor) = st.scan {
                    return Next::Refill {
                        cursor,
                        generation: st.scan_generation,
                    };
                }
            }
            st = self.cv.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Sleep up to `timeout`, waking early on shutdown. Used to back off after a refused render.
    pub(super) fn nap(&self, timeout: Duration) {
        let st = self.lock();
        if !st.stop {
            drop(self.cv.wait_timeout(st, timeout));
        }
    }
}

impl State {
    fn restart_scan(&mut self) {
        self.scan = Some(i64::MIN);
        self.scan_generation = self.scan_generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(queue: &Queue) -> Vec<i64> {
        let mut out = Vec::new();
        queue.refill(0, &[]); // ends the initial pass
        while let Next::Render(id) = queue.next() {
            out.push(id);
            if queue.lock().front.is_empty() && queue.lock().back.is_empty() {
                break;
            }
        }
        out
    }

    #[test]
    fn front_runs_before_back_and_newest_call_first() {
        let queue = Queue::new();
        queue.enqueue_back(&[10, 11]);
        queue.prioritize(&[1, 2]);
        queue.prioritize(&[3]);
        queue.prioritize(&[11]);
        assert_eq!(drain(&queue), vec![11, 3, 1, 2, 10]);
    }

    #[test]
    fn refill_pages_resume_by_cursor_and_ignore_stale_generation() {
        let queue = Queue::new();
        let page: Vec<i64> = (1..=PAGE_SIZE as i64).collect();
        queue.refill(0, &page);
        assert!(matches!(
            queue.lock().scan,
            Some(c) if c == PAGE_SIZE as i64
        ));
        queue.refill(9, &[999]); // stale pass
        assert!(!queue.lock().queued.contains(&999));
        queue.refill(0, &[PAGE_SIZE as i64 + 1]);
        assert!(queue.lock().scan.is_none());
    }
}
