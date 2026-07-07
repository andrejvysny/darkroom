//! Interactive segmentation (Lightroom-style AI masking) IPC backend: MobileSAM encode/decode plus
//! the baked-coverage cache that feeds the GPU mask pre-pass. Compiled on Apple Silicon + Windows;
//! the Intel-macOS build uses `segment_stub.rs`.
//!
//! Flow: `mask_ai_prompt` lazily builds the segmenter, encodes the image once (cached), decodes the
//! prompt to an alpha, and stashes it keyed by a prompt hash. The frontend writes that hash + the
//! prompt points into the mask's `ComponentKind::Ai`; `develop_render` then resolves the coverage via
//! [`coverages_for`] and hands it to the pipeline. On reload, `coverages_for` re-decodes from the
//! stored points so AI masks survive a session restart.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use core_analyze::models::{ModelStore, SamTier};
use core_analyze::sam::{Prompt, SamEmbedding, Segmenter};
use core_pipeline::{AiCoverage, AiPoint, ComponentKind, DevelopParams};
use tauri::{AppHandle, Manager, Runtime};

use crate::state::AppState;

/// The active segmentation tier, from the persisted Settings value (defaults to Realtime). Only
/// Realtime (MobileSAM) is functional today; Balanced/Max are gated off in the UI until their ONNX
/// exports are validated, so this normally resolves to Realtime regardless.
fn active_tier(st: &AppState) -> SamTier {
    let tag = st
        .db
        .lock()
        .ok()
        .and_then(|db| core_library::mask_ai_tier(&db.conn).ok())
        .unwrap_or_else(|| "realtime".to_string());
    SamTier::from_tag(&tag)
}

pub fn models_ready(st: &AppState) -> bool {
    ModelStore::new(st.models_dir.clone()).has_all(active_tier(st).files())
}

/// Download the active tier's SAM weights with byte-level `mask_ai:models` progress + cancellation.
/// Serialized so a background warm + a first click don't each start the same (up to 1.2 GB) download;
/// the second caller finds the files present and returns immediately.
pub fn ensure_models<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let _dl_guard = st.sam_download_lock.lock().map_err(|e| e.to_string())?;
    st.mask_ai_dl_cancel.store(false, Ordering::SeqCst);
    let store = ModelStore::new(st.models_dir.clone());
    let files = active_tier(&st).files();
    let cancel = || st.mask_ai_dl_cancel.load(Ordering::SeqCst);
    store
        .ensure_with(
            files,
            |p| crate::model_mgmt::emit_dl(app, "mask_ai:models", &p),
            &cancel,
        )
        .map_err(|e| e.to_string())
}

/// Human label for a SAM tier, shown in the manager's per-tier detail.
fn tier_label(t: SamTier) -> &'static str {
    match t {
        SamTier::Realtime => "Realtime · MobileSAM",
        SamTier::Balanced => "Balanced · SAM ViT-B",
        SamTier::Max => "Max · SAM ViT-L",
    }
}

/// Manager overview for the AI Masking capability. `installed`/size reflect the ACTIVE tier; `tiers`
/// carries the per-tier install state so a stale non-active tier can be reclaimed independently.
pub fn overview(st: &AppState) -> crate::model_mgmt::ModelGroup {
    use crate::model_mgmt::{file_info, ModelGroup, ModelTierInfo};
    let store = ModelStore::new(st.models_dir.clone());
    let active = active_tier(st);
    let tiers: Vec<ModelTierInfo> = SamTier::all()
        .iter()
        .map(|&t| {
            let installed = store.has_all(t.files());
            let approx: u64 = t.files().iter().map(|f| f.approx_size).sum();
            ModelTierInfo {
                tier: t.as_str().to_string(),
                label: tier_label(t).to_string(),
                installed,
                size_bytes: if installed {
                    store.installed_bytes(t.files())
                } else {
                    approx
                },
                files: t.files().iter().map(|f| file_info(&store, f)).collect(),
            }
        })
        .collect();
    let installed = store.has_all(active.files());
    let approx_total: u64 = active.files().iter().map(|f| f.approx_size).sum();
    ModelGroup {
        id: "mask_ai".into(),
        name: "AI Masking".into(),
        description: "Click a subject in Develop to auto-select it as a mask (SAM).".into(),
        available: true,
        installed,
        size_bytes: if installed {
            store.installed_bytes(active.files())
        } else {
            approx_total
        },
        approx_total_bytes: approx_total,
        license: None,
        files: active
            .files()
            .iter()
            .map(|f| file_info(&store, f))
            .collect(),
        tiers,
        active_tier: Some(active.as_str().to_string()),
        accelerator: core_analyze::accelerator().to_string(),
    }
}

/// Remove one SAM tier's files and clear the cached segmenter/embedding/masks (they rebuild lazily).
/// `tier` is a tag (`"realtime"`/`"balanced"`/`"max"`); empty falls back to the active tier.
pub fn remove(st: &AppState, tier: &str) -> Result<(), String> {
    let tier = if tier.is_empty() {
        active_tier(st)
    } else {
        SamTier::from_tag(tier)
    };
    let store = ModelStore::new(st.models_dir.clone());
    store.remove(tier.files()).map_err(|e| e.to_string())?;
    *st.segmenter.lock().map_err(|e| e.to_string())? = None;
    *st.sam_embedding.lock().map_err(|e| e.to_string())? = None;
    st.sam_masks.lock().map_err(|e| e.to_string())?.clear();
    Ok(())
}

/// Build (once) and cache the segmenter for the active tier. A tier change rebuilds it and clears the
/// stale embedding + baked masks.
fn segmenter(st: &AppState) -> Result<Arc<Segmenter>, String> {
    let tier = active_tier(st);
    {
        let g = st.segmenter.lock().map_err(|e| e.to_string())?;
        if let Some((t, seg)) = g.as_ref() {
            if *t == tier {
                return Ok(seg.clone());
            }
        }
    }
    if !models_ready(st) {
        return Err("AI mask models not downloaded".into());
    }
    let store = ModelStore::new(st.models_dir.clone());
    let seg = Arc::new(
        Segmenter::new(
            &store.sam_encoder_path(tier),
            &store.sam_decoder_path(tier),
            tier.preproc(),
            tier.encoder_cpu(),
        )
        .map_err(|e| e.to_string())?,
    );
    *st.segmenter.lock().map_err(|e| e.to_string())? = Some((tier, seg.clone()));
    *st.sam_embedding.lock().map_err(|e| e.to_string())? = None;
    st.sam_masks.lock().map_err(|e| e.to_string())?.clear();
    Ok(seg)
}

/// Oriented sRGB preview for `image_id` — the EXIF-uprighted frame the develop view displays, so
/// normalized prompt coords line up with what the user clicked.
fn decode_oriented(st: &AppState, image_id: i64) -> Result<image::RgbImage, String> {
    let path = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?
            .path
    };
    let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
    let (mut img, orientation) =
        core_raw::preview_with_orientation(&src).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img.to_rgb8())
}

/// Get-or-compute the cached SAM embedding for `image_id` (heavy encode reused across clicks).
/// Switching images clears the mask cache so another image's masks never leak in.
fn embedding(st: &AppState, image_id: i64) -> Result<Arc<SamEmbedding>, String> {
    // Fast path: cache hit, no lock contention.
    if let Some(emb) = cached_embedding(st, image_id)? {
        return Ok(emb);
    }
    // Serialize the (multi-second) encode so a background warm + a click don't both run it. Re-check
    // the cache after acquiring — the other holder may have just filled it.
    let _encode_guard = st.sam_encode_lock.lock().map_err(|e| e.to_string())?;
    if let Some(emb) = cached_embedding(st, image_id)? {
        return Ok(emb);
    }
    let seg = segmenter(st)?;
    let img = decode_oriented(st, image_id)?;
    let emb = Arc::new(seg.encode(&img).map_err(|e| e.to_string())?);
    *st.sam_embedding.lock().map_err(|e| e.to_string())? = Some((image_id, emb.clone()));
    st.sam_masks.lock().map_err(|e| e.to_string())?.clear();
    Ok(emb)
}

/// The cached embedding for `image_id`, if present.
fn cached_embedding(st: &AppState, image_id: i64) -> Result<Option<Arc<SamEmbedding>>, String> {
    let g = st.sam_embedding.lock().map_err(|e| e.to_string())?;
    Ok(g.as_ref().and_then(|(id, emb)| {
        if *id == image_id {
            Some(emb.clone())
        } else {
            None
        }
    }))
}

fn to_prompts(points: &[AiPoint]) -> Vec<Prompt> {
    points
        .iter()
        .map(|p| Prompt {
            x: p.x,
            y: p.y,
            positive: p.positive,
        })
        .collect()
}

/// Stable hash of a prompt set → hex string (the `ComponentKind::Ai` component key + cache key).
/// Deterministic over `(image_id, tier, points)` so a reload recomputes the same key from the stored
/// points and hits the same GPU coverage slot.
fn prompt_hash(image_id: i64, tier: SamTier, points: &[AiPoint]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    image_id.hash(&mut h);
    tier.as_str().hash(&mut h);
    for p in points {
        p.x.to_bits().hash(&mut h);
        p.y.to_bits().hash(&mut h);
        p.positive.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResult {
    /// Component key to write into the mask's `ComponentKind::Ai.hash`.
    pub hash: String,
    pub width: u32,
    pub height: u32,
    pub iou: f32,
}

/// Warm the per-image SAM embedding (the heavy encode) without decoding a prompt, so the first click
/// is instant. Idempotent — reuses the cached embedding for the same image.
pub fn warm(st: &AppState, image_id: i64) -> Result<(), String> {
    embedding(st, image_id).map(|_| ())
}

/// Decode a mask from prompt points and cache its baked alpha. Returns the component key + dims + IoU.
pub fn prompt(st: &AppState, image_id: i64, points: Vec<AiPoint>) -> Result<PromptResult, String> {
    if points.is_empty() {
        return Err("no prompt points".into());
    }
    let seg = segmenter(st)?;
    let emb = embedding(st, image_id)?;
    let mask = seg
        .decode(&emb, &to_prompts(&points), None, None)
        .map_err(|e| e.to_string())?;
    let hash = prompt_hash(image_id, active_tier(st), &points);
    let cov = AiCoverage {
        hash: hash.clone(),
        width: mask.width,
        height: mask.height,
        alpha: mask.alpha,
    };
    st.sam_masks
        .lock()
        .map_err(|e| e.to_string())?
        .insert(hash.clone(), cov);
    Ok(PromptResult {
        hash,
        width: mask.width,
        height: mask.height,
        iou: mask.iou,
    })
}

/// Resolve the baked AI coverages referenced by `params.masks`, for the GPU pre-pass. Cache-first;
/// on a miss (e.g. right after reload) it lazily re-decodes from the component's stored prompt points
/// so AI masks survive a restart. Returns empty (no decode cost) when there are no AI components.
pub fn coverages_for(st: &AppState, image_id: i64, params: &DevelopParams) -> Vec<AiCoverage> {
    let wanted: Vec<(String, Vec<AiPoint>)> = params
        .masks
        .iter()
        .flat_map(|m| m.components.iter())
        .filter_map(|c| match &c.kind {
            ComponentKind::Ai { hash, points, .. } if !hash.is_empty() && !points.is_empty() => {
                Some((hash.clone(), points.clone()))
            }
            _ => None,
        })
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let missing: Vec<(String, Vec<AiPoint>)> = {
        let cache = match st.sam_masks.lock() {
            Ok(c) => c,
            Err(_) => return out,
        };
        let mut miss = Vec::new();
        for (hash, pts) in wanted {
            match cache.get(&hash) {
                Some(cov) => out.push(cov.clone()),
                None => miss.push((hash, pts)),
            }
        }
        miss
    };
    // Lazy rebuild of misses (reload path). Best-effort: a failure just leaves that mask empty.
    if !missing.is_empty() {
        if let (Ok(seg), Ok(emb)) = (segmenter(st), embedding(st, image_id)) {
            for (hash, pts) in missing {
                if let Ok(mask) = seg.decode(&emb, &to_prompts(&pts), None, None) {
                    let cov = AiCoverage {
                        hash: hash.clone(),
                        width: mask.width,
                        height: mask.height,
                        alpha: mask.alpha,
                    };
                    if let Ok(mut cache) = st.sam_masks.lock() {
                        cache.insert(hash, cov.clone());
                    }
                    out.push(cov);
                }
            }
        }
    }
    out
}
