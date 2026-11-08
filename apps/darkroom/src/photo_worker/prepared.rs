//! The worker's prepared-upload cache and the resource/timing numbers it publishes.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use core_pipeline::backend::PreparedImage;

/// Nominal byte budget for prepared GPU images. One 32 MP image incl. its lazily allocated mask
/// array (~2.0 GB nominal) fits; two (~4.1 GB) do not, so the second prepare evicts the first. A
/// 60 MP image (~3.8 GB nominal incl. masks) exceeds it: it opens alone and flags `over_budget`,
/// as does a huge panorama; the GPU OOM error scope in `prepare` reports real failure. Nominal
/// allocation arithmetic, not measured memory.
pub(crate) const PREPARED_BUDGET_BYTES: u64 = 3 * 1024 * 1024 * 1024;

/// Most prepared images kept regardless of budget: the current photo, its previous one for a
/// quick step back, and the speculative ±1 neighbours.
const PREPARED_MAX_ENTRIES: usize = 4;

/// Cache key of one decoded + uploaded source: `(ImageId, content_hash)`. A re-indexed image with
/// new bytes never reuses a stale upload under the same id.
pub(crate) type SourceKey = (i64, String);

/// Payload with a (live) nominal resident byte size.
pub(crate) trait SizedPayload {
    fn bytes(&self) -> u64;
}

impl SizedPayload for PreparedImage {
    fn bytes(&self) -> u64 {
        self.resident_bytes().total()
    }
}

/// Wall-clock milliseconds each stage of the last request took. Reset at the start of every
/// render and published after the request, so a failed/abandoned request still reports the
/// stages it completed (the rest stay 0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StageTimings {
    /// `core_raw::source_from_path`.
    pub read_ms: u32,
    /// `core_raw::develop_linear`.
    pub develop_ms: u32,
    /// `pipeline.prepare`.
    pub prepare_ms: u32,
    /// The `render_until_free(...)` call.
    pub render_ms: u32,
    /// The histogram follow-up (runs after the frame was delivered).
    pub histogram_ms: u32,
    /// `prepare_image` returned a cached entry.
    pub prepared_cache_hit: bool,
    /// The photo came from the neighbour prefetch (decode handoff or speculative upload).
    pub prefetch_hit: bool,
}

pub(crate) fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// Nominal resource numbers (allocation arithmetic, not measured memory) published by the worker
/// after each request. Pool numbers come from `Renderer::resource_report`; prepared numbers are
/// summed separately and are NOT folded into the pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceReport {
    pub prepared_gpu_bytes: u64,
    pub prepared_count: usize,
    pub pool_bytes: u64,
    pub pool_limit_bytes: u64,
    /// CPU bytes of the most recent transient decoded linear image (freed after upload).
    pub last_decode_cpu_bytes: u64,
    pub peak_decode_cpu_bytes: u64,
    /// The last prepare needed more than the budget even after evicting everything else.
    pub over_budget: bool,
    /// Stage timings of the last request (zeros before the first).
    pub last_timings: StageTimings,
}

/// What the histogram follow-up needs: the frame it describes plus the same prepared image and
/// params. Computed only after the frame itself was delivered, and only when nothing newer waits.
pub(crate) struct HistogramJob<V = PreparedImage> {
    pub key: core_model::identity::FrameKey,
    pub prepared: Arc<V>,
    pub params: core_pipeline::DevelopParams,
}

/// Prepared-upload cache keyed by [`SourceKey`], bounded by BYTES (and a small entry cap). LRU:
/// front of `order` is least recent; `get` refreshes. Callers run [`Self::make_room`] BEFORE the
/// expensive prepare. Generic over the payload so unit tests need no GPU.
pub(crate) struct PreparedCache<V = PreparedImage> {
    pub(super) entries: HashMap<SourceKey, Arc<V>>,
    order: Vec<SourceKey>,
    pub(super) budget_bytes: u64,
    max_entries: usize,
    /// Source of the most recent render/warm request (the photo on screen): never evicted by
    /// another source's room-making.
    pinned: Option<SourceKey>,
    /// Entries uploaded by the prefetch and not yet used by a render/warm; the only entries a
    /// further speculative upload may evict.
    speculative: HashSet<SourceKey>,
    /// The last source whose first use was a speculative entry (for `prefetch_hit` reporting).
    prefetched: Option<SourceKey>,
    /// Where the prefetch parks decoded images; `prepare_image` promotes from it.
    pub(crate) handoff: Option<Arc<super::prefetch::Prefetch>>,
    pub(super) over_budget: bool,
    last_decode_cpu_bytes: u64,
    peak_decode_cpu_bytes: u64,
    /// Stage timings of the request in flight; reset at the start of a render, published by
    /// `report` (same pattern as `note_decode`).
    pub(crate) timings: StageTimings,
    /// The histogram owed for the frame just returned (see [`HistogramJob`]).
    pub(crate) histogram_due: Option<Box<HistogramJob<V>>>,
}

impl<V> PreparedCache<V> {
    pub(crate) fn new() -> Self {
        Self::with_limits(PREPARED_BUDGET_BYTES, PREPARED_MAX_ENTRIES)
    }

    pub(crate) fn with_limits(budget_bytes: u64, max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            budget_bytes,
            max_entries,
            pinned: None,
            speculative: HashSet::new(),
            prefetched: None,
            handoff: None,
            over_budget: false,
            last_decode_cpu_bytes: 0,
            peak_decode_cpu_bytes: 0,
            timings: StageTimings::default(),
            histogram_due: None,
        }
    }

    /// Lookup that refreshes recency.
    pub(crate) fn get(&mut self, key: &SourceKey) -> Option<&Arc<V>> {
        if self.entries.contains_key(key) {
            self.touch(key);
        }
        self.entries.get(key)
    }

    pub(crate) fn contains(&self, key: &SourceKey) -> bool {
        self.entries.contains_key(key)
    }

    fn touch(&mut self, key: &SourceKey) {
        if let Some(at) = self.order.iter().position(|k| k == key) {
            let k = self.order.remove(at);
            self.order.push(k);
        }
    }

    fn evict_lru(&mut self, keep: Option<&SourceKey>) -> bool {
        let Some(at) = self
            .order
            .iter()
            .position(|k| Some(k) != keep && Some(k) != self.pinned.as_ref())
        else {
            return false;
        };
        self.evict_at(at);
        true
    }

    fn evict_at(&mut self, at: usize) {
        let oldest = self.order.remove(at);
        self.entries.remove(&oldest);
        self.speculative.remove(&oldest);
        if self.prefetched.as_ref() == Some(&oldest) {
            self.prefetched = None;
        }
    }

    /// `key` is the photo being shown now (render or warm). Returns whether it came from the
    /// prefetch, so the request can report a prefetch hit.
    pub(crate) fn pin(&mut self, key: &SourceKey) -> bool {
        if self.speculative.remove(key) {
            self.prefetched = Some(key.clone());
        }
        self.pinned = Some(key.clone());
        self.prefetched.as_ref() == Some(key)
    }

    /// Every source the prefetch must not decode again: cached ones plus the pinned one.
    pub(crate) fn known_sources(&self) -> HashSet<SourceKey> {
        self.order
            .iter()
            .chain(self.pinned.iter())
            .cloned()
            .collect()
    }

    /// Insert a prefetched upload; evictable by later speculative admissions until used.
    pub(crate) fn put_speculative(&mut self, key: SourceKey, prepared: Arc<V>) {
        self.put(key.clone(), prepared);
        self.speculative.insert(key);
    }

    pub(crate) fn put(&mut self, key: SourceKey, prepared: Arc<V>) {
        if self.entries.insert(key.clone(), prepared).is_none() {
            self.order.push(key.clone());
        } else {
            self.touch(&key);
        }
        while self.order.len() > self.max_entries && self.evict_lru(Some(&key)) {}
    }

    pub(crate) fn note_decode(&mut self, cpu_bytes: u64) {
        self.last_decode_cpu_bytes = cpu_bytes;
        self.peak_decode_cpu_bytes = self.peak_decode_cpu_bytes.max(cpu_bytes);
    }

    pub(crate) fn report(&self, pool: render_wgpu::ResourceReport) -> ResourceReport
    where
        V: SizedPayload,
    {
        ResourceReport {
            prepared_gpu_bytes: self.resident_bytes(),
            prepared_count: self.entries.len(),
            pool_bytes: pool.pool_bytes,
            pool_limit_bytes: pool.pool_limit_bytes,
            last_decode_cpu_bytes: self.last_decode_cpu_bytes,
            peak_decode_cpu_bytes: self.peak_decode_cpu_bytes,
            over_budget: self.over_budget,
            last_timings: self.timings,
        }
    }
}

impl<V: SizedPayload> PreparedCache<V> {
    pub(super) fn resident_bytes(&self) -> u64 {
        self.entries.values().map(|v| v.bytes()).sum()
    }

    /// Evict LRU entries (never `keep`) until `incoming` more bytes fit the budget and a new entry
    /// fits the entry cap, or nothing evictable is left. An `incoming` that cannot fit even then
    /// still proceeds (a lone huge image must open) and sets `over_budget`.
    pub(crate) fn make_room(&mut self, incoming: u64, keep: Option<&SourceKey>) {
        while (self.resident_bytes().saturating_add(incoming) > self.budget_bytes
            || self.order.len() >= self.max_entries)
            && self.evict_lru(keep)
        {}
        self.over_budget = self.resident_bytes().saturating_add(incoming) > self.budget_bytes;
    }

    /// Room for a speculative upload of `incoming` bytes by evicting only other speculative
    /// entries. Never evicts the pinned or any used entry, never exceeds the byte budget or the
    /// entry cap (no `over_budget` for speculation). `false` = not admitted; nothing is evicted.
    pub(crate) fn admit_speculative(&mut self, incoming: u64) -> bool {
        let (mut kept, mut kept_bytes) = (0usize, 0u64);
        for key in &self.order {
            if !self.speculative.contains(key) {
                kept += 1;
                kept_bytes += self.entries[key].bytes();
            }
        }
        if kept_bytes.saturating_add(incoming) > self.budget_bytes || kept >= self.max_entries {
            return false;
        }
        while (self.resident_bytes().saturating_add(incoming) > self.budget_bytes
            || self.order.len() >= self.max_entries)
            && self.evict_speculative()
        {}
        true
    }

    fn evict_speculative(&mut self) -> bool {
        let Some(at) = self
            .order
            .iter()
            .position(|k| self.speculative.contains(k) && Some(k) != self.pinned.as_ref())
        else {
            return false;
        };
        self.evict_at(at);
        true
    }
}
