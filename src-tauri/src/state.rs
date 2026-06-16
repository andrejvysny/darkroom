use core_db::Db;
use core_library::ThumbCache;
use core_pipeline::backend::PreparedImage;
use core_pipeline::{DevelopPipeline, GpuContext, Histogram};
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime};

/// GPU device + the compiled develop pipeline. Optional — the library works without a GPU.
pub struct GpuRender {
    pub ctx: GpuContext,
    pub pipeline: DevelopPipeline,
}

/// One developed image's uploaded GPU resources, keyed by image id.
pub struct DevelopCache {
    pub image_id: i64,
    pub prepared: PreparedImage,
}

/// How many images' GPU resources to keep warm at once. Small (back/forward + A/B compare) so
/// VRAM/unified-memory stays bounded; each 1600 px preview is modest.
const DEVELOP_CACHE_CAP: usize = 3;

/// LRU of prepared develop images. Front = most-recently-used.
#[derive(Default)]
pub struct DevelopLru {
    entries: VecDeque<DevelopCache>,
}

impl DevelopLru {
    pub fn contains(&self, image_id: i64) -> bool {
        self.entries.iter().any(|c| c.image_id == image_id)
    }

    /// Fetch a prepared image, promoting it to most-recently-used.
    pub fn get(&mut self, image_id: i64) -> Option<&PreparedImage> {
        let pos = self.entries.iter().position(|c| c.image_id == image_id)?;
        if pos != 0 {
            let item = self.entries.remove(pos).expect("position is valid");
            self.entries.push_front(item);
        }
        self.entries.front().map(|c| &c.prepared)
    }

    /// Insert (or replace) an image, evicting the least-recently-used over capacity.
    pub fn put(&mut self, image_id: i64, prepared: PreparedImage) {
        self.entries.retain(|c| c.image_id != image_id);
        self.entries.push_front(DevelopCache { image_id, prepared });
        while self.entries.len() > DEVELOP_CACHE_CAP {
            self.entries.pop_back();
        }
    }
}

/// Managed application state.
pub struct AppState {
    pub db: Mutex<Db>,
    pub thumbs: ThumbCache,
    pub gpu: Option<GpuRender>,
    /// Warm GPU resources for recently-developed images.
    pub develop_cache: Mutex<DevelopLru>,
    /// Monotonic id of the latest render request; lets a render skip its expensive decode when a
    /// newer request has already superseded it.
    pub latest_render: AtomicU64,
    /// Histogram of the most recent successful render, for a reliable pull (the event can be missed).
    pub last_histogram: Mutex<Option<Histogram>>,
    /// FS watcher kept alive for the app's lifetime; dropping it stops watching. Set after setup.
    pub watcher: Mutex<Option<notify::RecommendedWatcher>>,
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
                eprintln!("[darkroom] GPU develop unavailable: {e}");
                None
            }
        };

        Ok(Self {
            db: Mutex::new(db),
            thumbs,
            gpu,
            develop_cache: Mutex::new(DevelopLru::default()),
            latest_render: AtomicU64::new(0),
            last_histogram: Mutex::new(None),
            watcher: Mutex::new(None),
        })
    }
}
