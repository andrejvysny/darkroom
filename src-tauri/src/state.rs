use crate::thumb_queue::ThumbQueue;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
use core_analyze::{AnalyzerRegistry, FaceAnalyzer, SamEmbedding, Segmenter};
use core_db::Db;
use core_library::ThumbCache;
use core_pipeline::backend::PreparedImage;
use core_pipeline::{DevelopPipeline, GpuContext, Histogram};
use core_raw::LinearImage;
use std::collections::HashSet;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{AppHandle, Manager, Runtime};

/// Cached `(image_id, clean, denoised)` full-res linear buffers backing fast denoise amount re-blends.
type DenoiseBuffers = (i64, Arc<LinearImage>, Arc<LinearImage>);

/// GPU device + the compiled develop pipeline. Optional — the library works without a GPU.
pub struct GpuRender {
    pub ctx: GpuContext,
    pub pipeline: DevelopPipeline,
}

/// Managed application state.
pub struct AppState {
    pub db: Mutex<Db>,
    pub thumbs: ThumbCache,
    pub gpu: Option<GpuRender>,
    /// Background queue that renders canonical develop thumbnails so the grid/filmstrip/loupe match
    /// the editor. The worker thread is spawned in setup; this is its shared control handle.
    pub thumb_queue: ThumbQueue,
    /// Single full-resolution prepared image for zoomed (1:1) develop rendering. Bounded to ONE
    /// entry since a full-res texture is large (~0.5 GB for a 32 MP frame); replaced on image change.
    /// `Arc` so renders clone the handle and DROP this lock before the GPU render+readback —
    /// the per-image serialization lives inside `PreparedImage` (`render_lock`). An evicted image
    /// stays alive until its in-flight render finishes (one transient extra texture).
    pub full_render_cache: Mutex<Option<(i64, Arc<PreparedImage>)>>,
    /// Single half-resolution prepared image used for fast first paint on fit/whole-crop views,
    /// especially useful on Windows where full-res upload/readback is expensive.
    pub preview_render_cache: Mutex<Option<(i64, Arc<PreparedImage>)>>,
    /// Cached full-res `(clean, denoised)` linear buffers for the currently-denoised image, so changing
    /// the blend amount re-lerps (+ re-uploads) instead of re-running the seconds-long denoise. Single
    /// entry (mirrors `full_render_cache`); cleared on image switch / `denoise_clear`. The applied
    /// denoised `PreparedImage` is swapped straight into `full_render_cache`, so the render path is
    /// unchanged — this only backs fast amount re-blends.
    pub denoise_cache: Mutex<Option<DenoiseBuffers>>,
    /// Lazily-built PMRID neural denoiser (its ort Session is reused across applies). `None` until the
    /// first denoise; stays `None` on builds without the embedded model (→ `CpuDenoiser` fallback).
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub denoiser: Mutex<Option<Arc<core_analyze::denoise::PmridDenoiser>>>,
    /// Guards against two denoise computes running at once.
    pub denoise_running: AtomicBool,
    /// Set by `denoise_cancel` to abort the running compute (checked between phases).
    pub denoise_cancel: AtomicBool,
    /// Guards against two panorama merges running at once.
    pub panorama_running: AtomicBool,
    /// Set by `panorama_cancel` to abort the running merge (checked between sources/phases).
    pub panorama_cancel: AtomicBool,
    /// Downscaled source frames cached for the merge dialog preview, so projection/crop toggles
    /// restitch without re-decoding the raws (see `panorama::PreviewCache`).
    pub panorama_preview_cache: crate::panorama::PreviewCacheSlot,
    /// Guards against two panorama-detection scans running at once.
    pub pano_detect_running: AtomicBool,
    /// Set by `pano_detect_cancel` to abort the running scan (checked between clusters).
    pub pano_detect_cancel: AtomicBool,
    /// Monotonic id of the latest render request; lets a render skip its expensive decode when a
    /// newer request has already superseded it.
    pub latest_render: AtomicU64,
    /// Monotonic id of the latest whole-crop histogram request; a stale (out-of-order) histogram
    /// result must not overwrite / emit over a newer one.
    pub hist_seq: AtomicU64,
    /// Image id of the most recent interactive develop render. Lets the background full-res warm
    /// thread abort (and never clobber the caches) once the user has switched images, while staying
    /// insensitive to slider moves on the SAME image.
    pub current_image: AtomicI64,
    /// Keys `(image_id, is_preview_tier)` of decodes currently in flight. Concurrent requests for
    /// the same tier wait on `decode_cv` for the first decode instead of duplicating the multi-
    /// second RAW decode (slider burst / histogram right after open).
    pub decode_inflight: Mutex<HashSet<(i64, bool)>>,
    pub decode_cv: Condvar,
    /// Byte-capped LRU of decoded half-res linear previews (the multi-second decode result; the GPU
    /// upload from it is ~100 ms). Filled by the interactive preview decode AND the predictive
    /// prefetch worker, so stepping through a collection hits warm CPU buffers instead of decoding.
    pub preview_linear_lru: Mutex<crate::prefetch::PreviewLru>,
    /// Generation of the newest `develop_prefetch` request; the worker aborts a stale set as soon
    /// as a newer one (or an image switch) arrives.
    pub prefetch_gen: AtomicU64,
    /// Memo of `(image_id, is_display_referred)` so the per-slider-move `develop_render` doesn't hit
    /// the DB to learn whether the source is a JPEG/PNG (recomputed only on image change).
    pub display_referred_memo: Mutex<Option<(i64, bool)>>,
    /// Single-flight guard for `hdr_merge` (a merge decodes N full-res RAWs — one at a time).
    pub hdr_running: AtomicBool,
    /// Set by `hdr_cancel` to abort the running merge (checked between frames). Reset on the merge's
    /// `Running` drop-guard so an early return / error can never leave it latched.
    pub hdr_cancel: AtomicBool,
    /// Histogram of the most recent successful render, for a reliable pull (the event can be missed).
    pub last_histogram: Mutex<Option<Histogram>>,
    /// FS watcher kept alive for the app's lifetime; dropping it stops watching. Set after setup.
    pub watcher: Mutex<Option<notify::RecommendedWatcher>>,
    /// Number of imports currently in flight. While > 0 the FS watcher defers its reconcile/index
    /// pass so it can't race an import's unlocked copy/process phase (double-decode / duplicate
    /// insert). A counter, not a bool, so overlapping imports compose. See `watch::ImportGuard`.
    pub import_active: AtomicUsize,
    /// Set by the watcher when it skipped a sync because an import was in flight; the import's
    /// completion guard then runs exactly one deferred sync to catch any real external change.
    pub watch_pending: AtomicBool,
    /// Directory holding downloaded ML model files (`<app-data>/models`).
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub models_dir: PathBuf,
    /// AI analyzer registry, lazily built on first analysis run (loading ~300 MB of ONNX is deferred
    /// until the user actually analyzes). `None` until then.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    /// Cached Phase-A registry plus the stage bitmask it was built for — a run selecting a different
    /// set of detectors must rebuild rather than silently reuse the wrong analyzers.
    pub analyzers: Mutex<Option<(u8, Arc<AnalyzerRegistry>)>>,
    /// Guards the unified scan job (model download → analysis → panorama) as ONE operation.
    /// Separate from `analysis_running` because the job starts before any pass does — that gap is
    /// exactly where a Stop during a model download used to be silently ignored.
    pub scan_running: AtomicBool,
    /// Cancels the unified scan job in whatever phase it is currently in.
    pub scan_cancel: AtomicBool,
    /// Guards against two analysis passes running at once.
    pub analysis_running: AtomicBool,
    /// Set by `analysis_cancel` to request the running pass stop between batches.
    pub analysis_cancel: AtomicBool,
    /// Aborts the face-clustering ("Finalising faces…") phase. Deliberately SEPARATE from
    /// `analysis_cancel`: clustering also runs *after* a cancelled scan, to place the faces that
    /// scan just found, and reusing the already-set analysis flag would abort it instantly.
    pub cluster_cancel: AtomicBool,
    /// Set by `models_cancel("analysis")` to abort an in-flight Detection & Scene model download
    /// (checked between read chunks). Reset at the start of each ensure.
    pub analysis_dl_cancel: AtomicBool,
    /// Set by `models_cancel("faces")` to abort an in-flight face-model download.
    pub faces_dl_cancel: AtomicBool,
    /// Set by `models_cancel("mask_ai")` to abort an in-flight SAM model download.
    pub mask_ai_dl_cancel: AtomicBool,
    /// Face detector + embedder (SCRFD + ArcFace, ~190 MB ONNX), lazily built on first scan with faces
    /// enabled. The face stage runs inside the unified scan, guarded by `analysis_running`.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub face_analyzer: Mutex<Option<Arc<FaceAnalyzer>>>,
    /// Interactive-segmentation (AI masking) MobileSAM encoder+decoder, lazily built on first AI-mask
    /// use. Paired with the tier its weights were loaded for so a Settings tier change rebuilds it.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub segmenter: Mutex<Option<(core_analyze::models::SamTier, Arc<Segmenter>)>>,
    /// Single cached SAM image embedding for the current develop image `(image_id, embedding)` — the
    /// heavy encode is reused across every click; recomputed on image switch (mirrors the single-entry
    /// `full_render_cache`). Cleared when the segmenter (tier) is rebuilt.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub sam_embedding: Mutex<Option<(i64, Arc<SamEmbedding>)>>,
    /// Baked AI-mask alpha coverages for the current image, keyed by the `ComponentKind::Ai` component
    /// `hash`. Populated by `mask_ai_prompt`; read by `develop_render` to feed the GPU pre-pass.
    /// Cleared on image switch (with `sam_embedding`) so stale masks from another image never leak.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub sam_masks: Mutex<std::collections::HashMap<String, core_pipeline::AiCoverage>>,
    /// Serializes SAM image encodes and dedups them: a background warm and a first click on the same
    /// image would otherwise both run the (multi-second) encoder. Holders re-check `sam_embedding`
    /// after acquiring, so the second caller returns the first's cached result instead of re-encoding.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub sam_encode_lock: Mutex<()>,
    /// Serializes SAM model downloads so a background warm and a first click don't each kick off the
    /// same (up to 1.2 GB) fetch concurrently. The first downloads; the second then sees the files
    /// present and skips.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub sam_download_lock: Mutex<()>,
    /// Serializes Detection & Scene model downloads (double-trigger safety, matching
    /// `sam_download_lock`): the second caller finds the files present and returns.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub analysis_download_lock: Mutex<()>,
    /// Serializes face-model downloads.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub faces_download_lock: Mutex<()>,
    /// Per-launch id stamped on every captured user-event (groups a usage session).
    pub session_id: String,
    /// App version stamped on events (label provenance / pipeline isolation).
    pub app_version: &'static str,
}

impl AppState {
    pub fn new<R: Runtime>(app: &AppHandle<R>) -> Result<Self, String> {
        let data_dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("app_data_dir: {e}"))?;
        std::fs::create_dir_all(&data_dir).map_err(|e| format!("create data dir: {e}"))?;

        // Map catalog-open failures to actionable, user-facing text — this string ends up in the
        // fatal startup dialog (`fatal_startup_error`), not just a log line.
        let db = Db::open(&data_dir.join("catalog.db")).map_err(|e| match e {
            core_db::DbError::SchemaTooNew { found, supported } => format!(
                "Your photo catalog was last opened by a newer version of Darkroom \
                 (catalog format v{found}; this version supports up to v{supported}).\n\n\
                 Install the latest Darkroom release to keep using this catalog. \
                 Your photos and edits are safe and untouched."
            ),
            core_db::DbError::Corrupt(detail) => format!(
                "The photo catalog failed its integrity check ({detail}).\n\n\
                 You can restore a recent copy from the \"backups\" folder next to the catalog \
                 in Application Support, or rebuild the catalog from your library's sidecar files."
            ),
            other => format!("The photo catalog could not be opened: {other}"),
        })?;
        let thumbs =
            ThumbCache::new(data_dir.join("thumbs")).map_err(|e| format!("thumb cache: {e}"))?;
        // Bound the cache to the configured cap on startup (best-effort).
        if let Ok(cap) = core_library::thumb_cache_cap(&db.conn) {
            let _ = thumbs.evict_to(cap);
        }

        // GPU init is best-effort: a missing/incompatible adapter must not break the library.
        let gpu = match GpuContext::new() {
            Ok(ctx) => {
                let pipeline = DevelopPipeline::new(&ctx);
                Some(GpuRender { ctx, pipeline })
            }
            Err(e) => {
                tracing::warn!(error = %crate::logging::safe_error(&e), "GPU develop unavailable");
                None
            }
        };

        #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
        let models_dir = data_dir.join("models");

        // Per-launch session id: start-millis + pid (no extra dep; unique enough for a local app).
        let start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let session_id = format!("{start_ms:x}-{}", std::process::id());

        Ok(Self {
            db: Mutex::new(db),
            thumbs,
            gpu,
            thumb_queue: ThumbQueue::new(),
            full_render_cache: Mutex::new(None),
            preview_render_cache: Mutex::new(None),
            denoise_cache: Mutex::new(None),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            denoiser: Mutex::new(None),
            denoise_running: AtomicBool::new(false),
            denoise_cancel: AtomicBool::new(false),
            panorama_running: AtomicBool::new(false),
            panorama_cancel: AtomicBool::new(false),
            panorama_preview_cache: Mutex::new(None),
            pano_detect_running: AtomicBool::new(false),
            pano_detect_cancel: AtomicBool::new(false),
            latest_render: AtomicU64::new(0),
            hist_seq: AtomicU64::new(0),
            current_image: AtomicI64::new(-1),
            decode_inflight: Mutex::new(HashSet::new()),
            decode_cv: Condvar::new(),
            preview_linear_lru: Mutex::new(crate::prefetch::PreviewLru::default()),
            prefetch_gen: AtomicU64::new(0),
            display_referred_memo: Mutex::new(None),
            hdr_running: AtomicBool::new(false),
            hdr_cancel: AtomicBool::new(false),
            last_histogram: Mutex::new(None),
            watcher: Mutex::new(None),
            import_active: AtomicUsize::new(0),
            watch_pending: AtomicBool::new(false),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            models_dir,
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            analyzers: Mutex::new(None),
            analysis_running: AtomicBool::new(false),
            analysis_cancel: AtomicBool::new(false),
            cluster_cancel: AtomicBool::new(false),
            scan_running: AtomicBool::new(false),
            scan_cancel: AtomicBool::new(false),
            analysis_dl_cancel: AtomicBool::new(false),
            faces_dl_cancel: AtomicBool::new(false),
            mask_ai_dl_cancel: AtomicBool::new(false),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            face_analyzer: Mutex::new(None),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            segmenter: Mutex::new(None),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            sam_embedding: Mutex::new(None),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            sam_masks: Mutex::new(std::collections::HashMap::new()),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            sam_encode_lock: Mutex::new(()),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            sam_download_lock: Mutex::new(()),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            analysis_download_lock: Mutex::new(()),
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            faces_download_lock: Mutex::new(()),
            session_id,
            app_version: env!("CARGO_PKG_VERSION"),
        })
    }
}
