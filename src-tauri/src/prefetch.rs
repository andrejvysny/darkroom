//! Predictive preview caching.
//!
//! Stepping through a collection (filmstrip next/next/next) must feel instant. The expensive step
//! per image is the RAW half-res decode (seconds); the GPU upload from a decoded buffer is ~100 ms.
//! So we keep a byte-capped LRU of decoded half-res `LinearImage`s on the CPU:
//!
//! - the interactive preview decode stores its result here (going BACK is instant), and
//! - `develop_prefetch` decodes the frontend-announced neighbors here in the background
//!   (going FORWARD is instant). Prefetch is CPU-only — it never touches the GPU queue, so it
//!   cannot contend with interactive renders.
//!
//! A newer prefetch request (or image switch) bumps `prefetch_gen`, aborting the stale set between
//! items. Decodes dedup against interactive ones via the shared `decode_inflight` claim.

use crate::state::AppState;
use core_raw::LinearImage;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// Cap the LRU by BYTES (dominant constraint; ~70–140 MB per half-res 24–33 MP preview) and by
/// entry count (bounds tiny-image pathologies).
const LRU_MAX_BYTES: usize = 768 * 1024 * 1024;
const LRU_MAX_ENTRIES: usize = 8;

/// Most-recently-used at the BACK. Small (≤ 8 entries) — linear scans are fine.
#[derive(Default)]
pub struct PreviewLru {
    entries: VecDeque<(i64, Arc<LinearImage>)>,
    bytes: usize,
}

fn image_bytes(img: &LinearImage) -> usize {
    img.data.len() * std::mem::size_of::<f32>()
}

impl PreviewLru {
    /// Fetch + promote to most-recently-used.
    pub fn get(&mut self, id: i64) -> Option<Arc<LinearImage>> {
        let pos = self.entries.iter().position(|(k, _)| *k == id)?;
        let entry = self.entries.remove(pos).expect("position just found");
        let img = entry.1.clone();
        self.entries.push_back(entry);
        Some(img)
    }

    pub fn contains(&self, id: i64) -> bool {
        self.entries.iter().any(|(k, _)| *k == id)
    }

    /// Insert (replacing any same-id entry), then evict least-recently-used past the caps.
    pub fn insert(&mut self, id: i64, img: Arc<LinearImage>) {
        if let Some(pos) = self.entries.iter().position(|(k, _)| *k == id) {
            let (_, old) = self.entries.remove(pos).expect("position just found");
            self.bytes -= image_bytes(&old);
        }
        self.bytes += image_bytes(&img);
        self.entries.push_back((id, img));
        while self.entries.len() > LRU_MAX_ENTRIES
            || (self.bytes > LRU_MAX_BYTES && self.entries.len() > 1)
        {
            if let Some((_, old)) = self.entries.pop_front() {
                self.bytes -= image_bytes(&old);
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }
}

/// Decode `image_id`'s half-res preview into the LRU (no GPU work). Returns `Ok(false)` when it
/// was skipped (already cached / claimed elsewhere and finished / superseded).
fn prefetch_one(st: &AppState, image_id: i64, my_gen: u64) -> Result<bool, String> {
    let stale = || st.prefetch_gen.load(Ordering::SeqCst) != my_gen;
    if stale() {
        return Ok(false);
    }
    {
        let lru = st.preview_linear_lru.lock().map_err(|e| e.to_string())?;
        if lru.contains(image_id) {
            return Ok(false);
        }
    }
    let present = || -> Result<bool, String> {
        Ok(st
            .preview_linear_lru
            .lock()
            .map_err(|e| e.to_string())?
            .contains(image_id))
    };
    let _claim = match crate::commands::claim_decode(st, (image_id, true), present, &stale)? {
        crate::commands::ClaimOutcome::Warm | crate::commands::ClaimOutcome::Superseded => {
            return Ok(false)
        }
        crate::commands::ClaimOutcome::Decode(claim) => claim,
    };
    let path = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?
            .path
    };
    let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
    // Even if the set moved on mid-decode, cache the result — it was a recent neighbor and the
    // LRU evicts it naturally if it stays cold.
    let lin = core_raw::develop_linear_preview(&src).map_err(|e| e.to_string())?;
    st.preview_linear_lru
        .lock()
        .map_err(|e| e.to_string())?
        .insert(image_id, Arc::new(lin));
    Ok(true)
}

/// Run one prefetch set in the background (spawned by `develop_prefetch`).
pub fn run_prefetch(app: AppHandle, image_ids: Vec<i64>, my_gen: u64) {
    std::thread::spawn(move || {
        let st = app.state::<AppState>();
        for id in image_ids {
            if st.prefetch_gen.load(Ordering::SeqCst) != my_gen {
                return; // a newer prefetch set (or image switch) superseded this one
            }
            match prefetch_one(st.inner(), id, my_gen) {
                Ok(true) => tracing::debug!(image_id = id, "prefetched preview"),
                Ok(false) => {}
                Err(e) => tracing::debug!(image_id = id, error = %e, "prefetch skipped"),
            }
        }
    });
}
