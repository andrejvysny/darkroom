//! IPC command handlers. Heavy/DB work runs on `spawn_blocking`; state is fetched via the
//! `AppHandle` inside the blocking closure (never held across `.await`).

use crate::state::{AppState, DevelopCache};
use core_library::{CollectionRow, FolderRow, ImageRow, IndexStats, KeywordRow, QueryParams};
use core_pipeline::DevelopParams;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tauri::{AppHandle, Emitter, Manager};

const PROCESS_VERSION: i64 = 1;

#[tauri::command]
pub async fn library_query(app: AppHandle, params: QueryParams) -> Result<Vec<ImageRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::query_images(&db.conn, &params).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_count(app: AppHandle, params: QueryParams) -> Result<i64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::count_images(&db.conn, &params).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_folders(app: AppHandle) -> Result<Vec<FolderRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_folders(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn image_meta(app: AppHandle, id: i64) -> Result<Option<ImageRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Index (or re-index) a folder as a watched root. Emits `import:progress`/`import:done`.
///
/// The DB lock is held only briefly (root upsert + known-path read, then the transactional
/// insert) — the multi-second parallel decode/hash/thumbnail work runs UNLOCKED so concurrent
/// `library_query` calls stay responsive during indexing.
#[tauri::command]
pub async fn library_index_root(app: AppHandle, path: String) -> Result<IndexStats, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();
        let root = PathBuf::from(&path);

        // --- brief lock: upsert folder + snapshot known paths ---
        let (folder_id, known) = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let fid = core_library::add_root(&db.conn, &root).map_err(|e| e.to_string())?;
            let known = core_library::existing_paths(&db.conn).map_err(|e| e.to_string())?;
            (fid, known)
        };

        // --- unlocked: enumerate + parallel process (hash + meta + thumbnail) ---
        let todo: Vec<PathBuf> = core_library::enumerate_raws(&root)
            .into_iter()
            .filter(|p| !known.contains(&p.display().to_string()))
            .collect();
        let total = todo.len();
        let done = AtomicUsize::new(0);
        let results: Vec<_> = todo
            .par_iter()
            .map(|p| {
                let r = core_library::process_file(p, &st.thumbs, core_library::THUMB_SIZE);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n == total || n.is_multiple_of(4) {
                    let _ = app2
                        .emit("import:progress", serde_json::json!({"done": n, "total": total}));
                }
                r
            })
            .collect();

        // --- brief lock: transactional insert ---
        let imported_at = core_library::now_epoch();
        let mut stats = IndexStats {
            scanned: total,
            ..Default::default()
        };
        {
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            for r in &results {
                match r {
                    Ok(p) => match core_library::insert_image(&tx, folder_id, imported_at, p)
                        .map_err(|e| e.to_string())?
                    {
                        Some(_) => stats.added += 1,
                        None => stats.skipped += 1,
                    },
                    Err(_) => stats.failed += 1,
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
        }

        let _ = app2.emit("import:done", &stats);
        Ok::<_, String>(stats)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Dev convenience: the bundled `library/2026` validation folder, if present on disk.
/// Returns `None` in a packaged app where that path does not exist.
#[tauri::command]
pub fn app_default_library() -> Option<String> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../library/2026");
    p.canonicalize()
        .ok()
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
}

// ---------- Develop ----------

/// Saved develop params for an image (defaults if none).
#[tauri::command]
pub async fn develop_get_edit(app: AppHandle, image_id: i64) -> Result<DevelopParams, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let params = match core_library::get_edit(&db.conn, image_id).map_err(|e| e.to_string())? {
            Some(json) => serde_json::from_str::<DevelopParams>(&json).unwrap_or_default(),
            None => DevelopParams::default(),
        };
        Ok::<_, String>(params)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Persist develop params (non-destructive; originals are never touched).
#[tauri::command]
pub async fn develop_set_edit(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let json = serde_json::to_string(&params).map_err(|e| e.to_string())?;
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_edit(
            &db.conn,
            image_id,
            PROCESS_VERSION,
            &json,
            core_library::now_epoch(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Render the develop preview for `image_id` with `params`, returning JPEG bytes.
/// First open of an image decodes + uploads (slow once); subsequent slider renders reuse the
/// cached GPU resources (single-digit ms).
#[tauri::command]
pub async fn develop_render(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
) -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let gpu = st
            .gpu
            .as_ref()
            .ok_or_else(|| "GPU develop unavailable".to_string())?;

        let mut cache = st.develop_cache.lock().map_err(|e| e.to_string())?;
        let needs_load = cache.as_ref().map(|c| c.image_id) != Some(image_id);
        if needs_load {
            let path = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::image_by_id(&db.conn, image_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "image not found".to_string())?
                    .path
            };
            let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
            let lin = core_raw::develop_linear(&src).map_err(|e| e.to_string())?;
            let preview = lin.downscaled(1600);
            let prepared = gpu
                .pipeline
                .prepare(&gpu.ctx, &preview)
                .map_err(|e| e.to_string())?;
            *cache = Some(DevelopCache { image_id, prepared });
        }

        let c = cache.as_ref().expect("cache populated above");
        let (w, h) = (c.prepared.width, c.prepared.height);
        let rgba = gpu
            .pipeline
            .render(&gpu.ctx, &c.prepared, &params)
            .map_err(|e| e.to_string())?;
        drop(cache);

        // Histogram from the actual rendered buffer → drives the develop panel histogram.
        let _ = app.emit("develop:histogram", core_pipeline::histogram(&rgba));

        let jpeg = core_pipeline::rgba8_to_jpeg(&rgba, w, h, 88).map_err(|e| e.to_string())?;
        Ok::<_, String>(tauri::ipc::Response::new(jpeg))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Export ----------

/// Export a single image at full resolution through the develop pipeline to `dest`.
/// `format` is "png" or "jpeg". Originals are never modified.
#[tauri::command]
pub async fn export_image(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
    format: String,
    dest: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let gpu = st
            .gpu
            .as_ref()
            .ok_or_else(|| "GPU export unavailable".to_string())?;

        let path = {
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::image_by_id(&db.conn, image_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "image not found".to_string())?
                .path
        };

        let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
        let lin = core_raw::develop_linear(&src).map_err(|e| e.to_string())?;
        let rgba = gpu
            .pipeline
            .render_once(&gpu.ctx, &lin, &params)
            .map_err(|e| e.to_string())?;

        let bytes = match format.to_lowercase().as_str() {
            "png" => core_pipeline::rgba8_to_png(&rgba, lin.width, lin.height),
            "jpeg" | "jpg" => core_pipeline::rgba8_to_jpeg(&rgba, lin.width, lin.height, 92),
            other => return Err(format!("unsupported export format: {other}")),
        }
        .map_err(|e| e.to_string())?;

        std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Culling ----------

async fn db_write<F>(app: AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(&core_db::rusqlite::Connection) -> Result<(), core_library::LibError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        f(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_rating(app: AppHandle, image_id: i64, stars: i64) -> Result<(), String> {
    db_write(app, move |c| core_library::set_rating(c, image_id, stars)).await
}

#[tauri::command]
pub async fn cull_set_flag(app: AppHandle, image_id: i64, flag: String) -> Result<(), String> {
    db_write(app, move |c| core_library::set_flag(c, image_id, &flag)).await
}

#[tauri::command]
pub async fn cull_set_label(
    app: AppHandle,
    image_id: i64,
    label: Option<String>,
) -> Result<(), String> {
    db_write(app, move |c| core_library::set_label(c, image_id, label.as_deref())).await
}

// Batch culling — applies one value to a whole selection in a single transaction.

#[tauri::command]
pub async fn cull_set_rating_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    stars: i64,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_rating_many(&mut db.conn, &image_ids, stars).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_flag_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    flag: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_flag_many(&mut db.conn, &image_ids, &flag).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_label_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    label: Option<String>,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_label_many(&mut db.conn, &image_ids, label.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Keywords / tags ----------

#[tauri::command]
pub async fn keywords_list(app: AppHandle) -> Result<Vec<KeywordRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_keywords(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keywords_for_image(app: AppHandle, image_id: i64) -> Result<Vec<KeywordRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::keywords_for_image(&db.conn, image_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_add_to_image(
    app: AppHandle,
    image_id: i64,
    name: String,
) -> Result<KeywordRow, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::add_keyword_to_image(&db.conn, image_id, &name).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_add_to_images(
    app: AppHandle,
    image_ids: Vec<i64>,
    name: String,
) -> Result<KeywordRow, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::add_keyword_to_images(&db.conn, &image_ids, &name).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_remove_from_image(
    app: AppHandle,
    image_id: i64,
    keyword_id: i64,
) -> Result<(), String> {
    db_write(app, move |c| {
        core_library::remove_keyword_from_image(c, image_id, keyword_id)
    })
    .await
}

#[tauri::command]
pub async fn keyword_delete(app: AppHandle, keyword_id: i64) -> Result<(), String> {
    db_write(app, move |c| core_library::delete_keyword(c, keyword_id)).await
}

// ---------- Collections ----------

#[tauri::command]
pub async fn collections_list(app: AppHandle) -> Result<Vec<CollectionRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::list_collections(&db.conn).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collections_for_image(
    app: AppHandle,
    image_id: i64,
) -> Result<Vec<CollectionRow>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::collections_for_image(&db.conn, image_id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_create(
    app: AppHandle,
    name: String,
    is_smart: bool,
    query: Option<String>,
) -> Result<i64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::create_collection(&db.conn, &name, is_smart, query.as_deref())
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_rename(app: AppHandle, id: i64, name: String) -> Result<(), String> {
    db_write(app, move |c| core_library::rename_collection(c, id, &name)).await
}

#[tauri::command]
pub async fn collection_delete(app: AppHandle, id: i64) -> Result<(), String> {
    db_write(app, move |c| core_library::delete_collection(c, id)).await
}

#[tauri::command]
pub async fn collection_add_images(
    app: AppHandle,
    collection_id: i64,
    image_ids: Vec<i64>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::add_images_to_collection(&db.conn, collection_id, &image_ids)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_remove_images(
    app: AppHandle,
    collection_id: i64,
    image_ids: Vec<i64>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::remove_images_from_collection(&db.conn, collection_id, &image_ids)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A sensible default import destination: the parent of the first watched folder (the library root
/// under which dated `YYYY/YYYY-MM-DD` folders live), if any.
#[tauri::command]
pub async fn app_library_root(app: AppHandle) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let first: Option<String> = db
            .conn
            .query_row("SELECT path FROM folders ORDER BY id LIMIT 1", [], |r| r.get(0))
            .ok();
        Ok::<_, String>(first.and_then(|p| {
            Path::new(&p)
                .parent()
                .map(|parent| parent.display().to_string())
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Dedup ----------

#[tauri::command]
pub async fn dedup_scan(
    app: AppHandle,
    category: String,
) -> Result<Vec<core_dedup::DupGroup>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        match category.as_str() {
            "capture" => core_dedup::find_same_capture(&db.conn),
            _ => core_dedup::find_byte_identical(&db.conn),
        }
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn dedup_resolve(
    app: AppHandle,
    keep_id: i64,
    trash_ids: Vec<i64>,
) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_dedup::resolve(&db.conn, keep_id, &trash_ids).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Import ----------

#[tauri::command]
pub async fn import_start(
    app: AppHandle,
    source: String,
    mode: String,
    dest: String,
) -> Result<core_import::ImportStats, String> {
    let app2 = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let st = app2.state::<AppState>();
        let import_mode = match mode.as_str() {
            "move" => core_import::ImportMode::Move,
            "reference" => core_import::ImportMode::Reference,
            _ => core_import::ImportMode::Copy,
        };
        let progress_app = app2.clone();
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let stats = core_import::import(
            &mut db.conn,
            &st.thumbs,
            Path::new(&source),
            import_mode,
            Path::new(&dest),
            |done, total| {
                if done == total || done.is_multiple_of(4) {
                    let _ = progress_app
                        .emit("import:progress", serde_json::json!({"done": done, "total": total}));
                }
            },
        )
        .map_err(|e| e.to_string())?;
        drop(db);
        let _ = app2.emit("import:done", &stats);
        Ok::<_, String>(stats)
    })
    .await
    .map_err(|e| e.to_string())?
}
