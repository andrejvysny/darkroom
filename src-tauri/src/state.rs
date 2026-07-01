use crate::thumb_queue::ThumbQueue;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
use core_analyze::{AnalyzerRegistry, FaceAnalyzer};
use core_db::Db;
use core_library::ThumbCache;
use core_pipeline::backend::PreparedImage;
use core_pipeline::{DevelopPipeline, GpuContext, Histogram};
use std::collections::HashSet;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize};
use std::sync::{Arc, Condvar, Mutex};
use tauri::{AppHandle, Manager, Runtime};

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
    pub analyzers: Mutex<Option<Arc<AnalyzerRegistry>>>,
    /// Guards against two analysis passes running at once.
    pub analysis_running: AtomicBool,
    /// Set by `analysis_cancel` to request the running pass stop between batches.
    pub analysis_cancel: AtomicBool,
    /// Face detector + embedder (SCRFD + ArcFace, ~190 MB ONNX), lazily built on first scan with faces
    /// enabled. The face stage runs inside the unified scan, guarded by `analysis_running`.
    #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
    pub face_analyzer: Mutex<Option<Arc<FaceAnalyzer>>>,
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

        let db =
            Db::open(&data_dir.join("catalog.db")).map_err(|e| format!("open catalog: {e}"))?;
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
            latest_render: AtomicU64::new(0),
            hist_seq: AtomicU64::new(0),
            current_image: AtomicI64::new(-1),
            decode_inflight: Mutex::new(HashSet::new()),
            decode_cv: Condvar::new(),
            preview_linear_lru: Mutex::new(crate::prefetch::PreviewLru::default()),
            prefetch_gen: AtomicU64::new(0),
            display_referred_memo: Mutex::new(None),
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
            #[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
            face_analyzer: Mutex::new(None),
            session_id,
            app_version: env!("CARGO_PKG_VERSION"),
        })
    }
}
