use core_db::Db;
use core_library::ThumbCache;
use core_pipeline::backend::PreparedImage;
use core_pipeline::{DevelopPipeline, GpuContext};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime};

/// GPU device + the compiled develop pipeline. Optional — the library works without a GPU.
pub struct GpuRender {
    pub ctx: GpuContext,
    pub pipeline: DevelopPipeline,
}

/// The currently-open develop image: its uploaded GPU resources keyed by image id.
pub struct DevelopCache {
    pub image_id: i64,
    pub prepared: PreparedImage,
}

/// Managed application state.
pub struct AppState {
    pub db: Mutex<Db>,
    pub thumbs: ThumbCache,
    pub gpu: Option<GpuRender>,
    pub develop_cache: Mutex<Option<DevelopCache>>,
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
            develop_cache: Mutex::new(None),
        })
    }
}
