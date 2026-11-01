//! IPC command handlers. Heavy/DB work runs on `spawn_fallible`/`spawn_job`; state is fetched via the
//! `AppHandle` inside the blocking closure (never held across `.await`).

use crate::state::{AppState, GpuRender};
use core_app::ports::CatalogPort;
use core_library::{
    CaptionRow, CollectionRow, DateTreeYear, DetectionRow, FacetRow, FolderRow, ImageFaceRow,
    ImageRow, IndexStats, KeywordRow, MergeSources, PersonFaceRow, PersonRow, PresenceRow,
    QueryParams, UserLabels,
};
use core_pipeline::{DevelopParams, Histogram};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

// The legacy settings-bridge commands live in their own module but are re-exported through
// `commands::` — the parity handler scanner (xtask/src/parity.rs) registers every command under
// this path, and `generate_handler!` entries must read `commands::<name>,`. The `__cmd__` /
// `__tauri_command_name_` items are the `#[tauri::command]` macro's companions, which the
// handler macro resolves through this module too.
pub use crate::settings_bridge::{
    __cmd__settings_bridge_export, __cmd__settings_bridge_write,
    __tauri_command_name_settings_bridge_export, __tauri_command_name_settings_bridge_write,
    settings_bridge_export, settings_bridge_write,
};

#[tauri::command]
pub fn frontend_log(
    level: String,
    target: String,
    message: String,
    fields: Option<serde_json::Value>,
) {
    crate::logging::frontend_log(&level, &target, &message, fields);
}

// v2 adds local adjustment masks (DevelopParams.masks). v1 rows deserialize with masks: [].
// v3 adds the scene-referred base tone operator (DevelopParams.tone_amount, default 100 = full ACR);
// it replaces the fixed highlight shoulder, so v1/v2 rows re-render with the new tonality.
// v4 fits the base tone curve to the real ACR default (mid-grey 0.18→0.388 ≈65% sRGB, ~+1.3 EV
// brighter) + adds Color-balance-RGB; all prior rows re-render with the matched ACR brightness.
// v5 changes the decode itself (core-raw): clipped-highlight reconstruction (no more magenta blown
// skies), dual-illuminant camera-matrix interpolation, rawler 0.8.0 demosaic. Canonical thumbnails
// and previews re-render; stored edit params are untouched.
// v6 fixes sequential local-mask composition and bounded tone-curve extrema. Saved parameters
// remain intact; both canonical and edited caches now include this renderer version.
/// Aliased to `core_model::identity::CURRENT_PROCESS_VERSION` so the native state machine and the
/// legacy host can never drift apart on the number that keys render caches and stored edits.
pub const PROCESS_VERSION: i64 = core_model::identity::CURRENT_PROCESS_VERSION.0;

/// Default cap on the Unsupported list — enough to see every camera the library trips over without
/// shipping a 50k-row table to the frontend.
const DECODE_FAILURE_LIMIT: i64 = 500;

/// Evict thumbnails down to the configured cache cap (best-effort). The cap is read under a brief
/// lock, then released before the (slower) filesystem eviction runs.
pub(crate) fn enforce_thumb_cap(st: &AppState) {
    let cap = {
        let Ok(db) = st.db.lock() else { return };
        core_library::thumb_cache_cap(&db.conn).unwrap_or(core_library::DEFAULT_THUMB_CACHE_CAP)
    };
    let _ = st.thumbs.evict_to(cap);
}

#[tauri::command]
pub async fn library_query(app: AppHandle, params: QueryParams) -> Result<Vec<ImageRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "library_query",
        move || {
            let st = app.state::<AppState>();
            st.catalog.query(&params).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_count(app: AppHandle, params: QueryParams) -> Result<i64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "library_count",
        move || {
            let st = app.state::<AppState>();
            st.catalog.count(&params).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_folders(app: AppHandle) -> Result<Vec<FolderRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "library_folders",
        move || {
            let st = app.state::<AppState>();
            st.catalog.folders().map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn library_date_tree(app: AppHandle) -> Result<Vec<DateTreeYear>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "library_date_tree",
        move || {
            let st = app.state::<AppState>();
            st.catalog.dates().map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn image_meta(app: AppHandle, id: i64) -> Result<Option<ImageRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "image_meta",
        move || {
            let st = app.state::<AppState>();
            st.catalog.image(id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn gpu_status(app: AppHandle) -> Option<core_pipeline::GpuAdapterInfo> {
    app.state::<AppState>()
        .gpu
        .as_ref()
        .map(|gpu| gpu.ctx.info.clone())
}

/// Index (or re-index) a folder as a watched root. Emits `import:progress`/`import:done`.
///
/// The DB lock is held only briefly (root upsert + known-path read, then the transactional
/// insert) — the multi-second parallel decode/hash/thumbnail work runs UNLOCKED so concurrent
/// `library_query` calls stay responsive during indexing.
///
/// Cancellable and non-durable: a cancel or shutdown before the single commit writes no catalog
/// rows at all (thumbnail files are a content-addressed cache), so shutdown never waits for a scan.
#[tauri::command]
pub async fn library_index_root(app: AppHandle, path: String) -> Result<IndexStats, String> {
    let app2 = app.clone();
    crate::jobs::spawn_job(
        app.clone(),
        core_app::jobs::JobClass::Import,
        false,
        "library_index_root",
        move |ctx| {
            let st = app2.state::<AppState>();
            index_root_blocking(&app2, &st, &PathBuf::from(&path), true, ctx)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// The RAW files the catalog could NOT take, most recently seen first — what the "Unsupported"
/// list renders. These paths have no `images` row by definition, so they are keyed by path.
#[tauri::command]
pub async fn decode_failures_list(
    app: AppHandle,
    limit: Option<i64>,
) -> Result<Vec<core_library::DecodeFailureRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "decode_failures_list",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::list_decode_failures(&db.conn, limit.unwrap_or(DECODE_FAILURE_LIMIT))
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Failure tallies by class, for the sidebar badge (`unsupported` / `corrupt` / `other`).
#[tauri::command]
pub async fn decode_failures_counts(
    app: AppHandle,
) -> Result<core_library::DecodeFailureCounts, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "decode_failures_counts",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::decode_failure_counts(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Forget the recorded failures for `paths` ("Try again"), so the next scan decodes them again.
/// Returns how many rows were removed. Touches only bookkeeping — no file is read or moved.
#[tauri::command]
pub async fn decode_failures_forget(app: AppHandle, paths: Vec<String>) -> Result<usize, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "decode_failures_forget",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::forget_decode_failures(&db.conn, &paths).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Index every supported RAW under `root` into the catalog: upsert the folder, enumerate
/// (recursively), sequential hash+meta+thumbnail, then a single transactional insert. Emits
/// `import:progress` / `import:done`. When `analyze`, kicks off background AI analysis of the
/// newly-added images (only if models are already downloaded). Runs synchronously on the caller's
/// (blocking) thread; `ctx` cancels it before anything is written and marks the single commit.
fn index_root_blocking(
    app: &AppHandle,
    st: &AppState,
    root: &Path,
    analyze: bool,
    ctx: &core_runtime::jobs::JobContext,
) -> Result<IndexStats, String> {
    // Captured before the first lock: every later catalog write is fenced on this generation.
    let epoch = st.publications.epoch();

    // --- brief lock: upsert folder + snapshot known paths + still-standing decode failures ---
    let (folder_id, known, known_bad) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let fid = core_library::add_root(&db.conn, root).map_err(|e| e.to_string())?;
        let known = core_library::existing_paths(&db.conn).map_err(|e| e.to_string())?;
        let known_bad = core_library::skippable_failures(&db.conn).map_err(|e| e.to_string())?;
        (fid, known, known_bad)
    };

    // --- unlocked: enumerate + sequential process (hash + meta + thumbnail) ---
    // Files whose recorded failure still stands (same decoder build, same bytes) are not re-opened.
    let mut skipped_bad = 0usize;
    let todo: Vec<PathBuf> = core_library::enumerate_raws(root, true)
        .into_iter()
        .filter(|p| !known.contains(&p.display().to_string()))
        .filter(|p| {
            let bad = known_bad.contains(&p.display().to_string());
            skipped_bad += usize::from(bad);
            !bad
        })
        .collect();
    let total = todo.len();
    let done = AtomicUsize::new(0);
    // The path rides along with its result: a `LibError` carries no path, and every failure has to
    // be recorded against the file that produced it.
    let mut results = Vec::with_capacity(total);
    for p in &todo {
        // Nothing has reached the catalog yet (thumbnails are a content-addressed cache), so a
        // cancellation here is a clean "nothing was written" exit.
        if ctx.is_cancel_requested() || st.workers.is_stopping() {
            return Err("indexing cancelled before commit; nothing was written".to_string());
        }
        let r = core_library::process_file(p, &st.thumbs, core_library::THUMB_SIZE);
        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
        if n == total || n.is_multiple_of(4) {
            let _ = app.emit(
                "import:progress",
                serde_json::json!({"done": n, "total": total}),
            );
        }
        results.push((p, r));
    }

    // --- fenced transactional insert: publication gate BEFORE the catalog lock (lock order) ---
    let imported_at = core_library::now_epoch();
    let mut stats = IndexStats {
        scanned: total,
        skipped: skipped_bad,
        ..Default::default()
    };
    {
        // The fence covers ONLY the catalog lock + transaction. Nothing after `tx.commit()`
        // may hold the global publication gate: thumbnail eviction, emits and queueing are IO
        // and must not stall every other sidecar/thumbnail/analysis publication in the app.
        // `images.id` has no AUTOINCREMENT, so an inserted id may be a rowid retired by an
        // earlier delete; the fence that made the write atomic also registers its identity.
        let mut fence = st
            .publications
            .commit_guard(epoch)
            .map_err(|_| "catalog was replaced while indexing; nothing was written".to_string())?;
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        let tx = db.conn.transaction().map_err(|e| e.to_string())?;
        let mut inserted: Vec<(i64, String, String)> = Vec::new();
        for (path, r) in &results {
            match r {
                Ok(p) => match core_library::insert_image(&tx, folder_id, imported_at, p)
                    .map_err(|e| e.to_string())?
                {
                    Some(id) => {
                        stats.added += 1;
                        inserted.push((id, p.content_hash_hex.clone(), p.path.clone()));
                        // A file that used to fail and now indexes leaves no stale record behind.
                        let _ = core_library::clear_decode_failure(&tx, &p.path);
                    }
                    None => stats.skipped += 1,
                },
                Err(e) => {
                    stats.failed += 1;
                    // Best-effort: a bookkeeping write must never fail a whole scan.
                    if core_library::record_decode_failure(
                        &tx,
                        Some(folder_id),
                        path,
                        e,
                        imported_at,
                    )
                    .unwrap_or(false)
                    {
                        stats.unsupported += 1;
                    }
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        for (id, hash_hex, path) in &inserted {
            fence.replace_source(
                core_model::identity::ImageId(*id),
                core_runtime::publication::SourceKey {
                    hash: hash_hex.clone(),
                    path: path.clone(),
                },
            );
        }
        // Durable from here on: a later cancellation ends `Interrupted` and still delivers stats.
        ctx.mark_committed();
    }

    enforce_thumb_cap(st);
    let _ = app.emit("import:done", &stats);
    // Backfill canonical develop thumbnails for the newly indexed images (idempotent: already-cached
    // ids are cheaply skipped by the worker's pre-check).
    crate::thumb_queue::enqueue_all(app);

    // The AI scan is fully MANUAL — one unified pass triggered from the UI. Nothing auto-runs after
    // import, so a 10k–100k library never kicks off a multi-hour background job on its own.
    let _ = analyze;
    Ok(stats)
}

/// Wipe the catalog to a fully empty state. Deletes every DB row (images, folders/watched roots,
/// edits, collections, keywords, analysis, behavioral events…) and clears the thumbnail + warm GPU
/// caches. Files on disk are never touched — only the index/metadata is removed. Nothing is
/// re-scanned: the app is left empty until the user imports again. Returns zeroed stats.
#[tauri::command]
pub async fn database_reset(app: AppHandle) -> Result<IndexStats, String> {
    let app2 = app.clone();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "database_reset",
        move || {
            let st = app2.state::<AppState>();
            // Catalog wipe + global retirement stay under the guard (lock order: gate, then db).
            {
                let _sources = st.publications.invalidate_all_guard();
                st.clear_source_caches();
                // Wipe all catalog rows (folders included → no watched roots remain).
                let mut db = st.db.lock().map_err(|e| e.to_string())?;
                db.wipe().map_err(|e| e.to_string())?;
            }
            // Clear derived caches (thumbnail files on disk + warm GPU resources + last histogram).
            // The eviction runs after the guard and db lock are dropped: the thumbnail cache is
            // content-addressed, so a concurrent thumbnail write after the wipe is harmless, and
            // bulk file deletes must not hold the catalog hostage.
            let _ = st.thumbs.evict_to(0);
            if let Ok(mut slot) = st.full_render_cache.lock() {
                *slot = None;
            }
            if let Ok(mut slot) = st.preview_render_cache.lock() {
                *slot = None;
            }
            if let Ok(mut lru) = st.preview_linear_lru.lock() {
                lru.clear();
            }
            if let Ok(mut h) = st.last_histogram.lock() {
                *h = None;
            }

            Ok::<_, String>(IndexStats::default())
        },
    )
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

/// `true` when a stored develop-edit blob exists but no longer parses into the current
/// `DevelopParams` schema (corruption, or a breaking schema change). Callers must never overwrite
/// such a blob without an explicit user Reset — that would destroy the real adjustments.
pub(crate) fn stored_edit_is_unreadable(stored: Option<&str>) -> bool {
    matches!(stored, Some(json) if serde_json::from_str::<DevelopParams>(json).is_err())
}

/// Saved develop params for an image (defaults if none).
#[tauri::command]
pub async fn develop_get_edit(app: AppHandle, image_id: i64) -> Result<DevelopParams, String> {
    crate::jobs::spawn_fallible(app.clone(), core_app::jobs::JobClass::UserCommand, true, "develop_get_edit",
        move || {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let params = match core_library::get_edit(&db.conn, image_id).map_err(|e| e.to_string())? {
            Some(json) => serde_json::from_str::<DevelopParams>(&json).unwrap_or_else(|e| {
                // Don't silently default — surface the parse failure. The stored blob is left intact
                // on disk; `develop_set_edit` refuses to overwrite an unreadable blob (unless forced
                // by an explicit Reset), so the user's real edit isn't destroyed by the next auto-save.
                tracing::warn!(image_id, error = %e, "develop params failed to parse; preserving stored edit");
                DevelopParams::default()
            }),
            None => DevelopParams::default(),
        };
        Ok::<_, String>(params)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `true` when this image has a stored develop edit that no longer parses into the current schema.
/// Develop calls this on open: the sliders show defaults in that case, so without it the UI would
/// silently invite edits that `develop_set_edit` then refuses. Cheap — one row read.
#[tauri::command]
pub async fn develop_edit_status(app: AppHandle, image_id: i64) -> Result<bool, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "develop_edit_status",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let stored = core_library::get_edit(&db.conn, image_id).map_err(|e| e.to_string())?;
            Ok::<_, String>(stored_edit_is_unreadable(stored.as_deref()))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Persist develop params (non-destructive; originals are never touched).
#[tauri::command]
pub async fn develop_set_edit(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
    touch_count: Option<i64>,
    // `true` to overwrite even an unreadable stored blob (explicit Reset). Defaults to `false`.
    force: Option<bool>,
) -> Result<i64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "develop_set_edit",
        move || {
            let json = serde_json::to_string(&params).map_err(|e| e.to_string())?;
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            // Snapshot the prior params for the event before overwriting.
            let before = core_library::get_edit(&db.conn, image_id).map_err(|e| e.to_string())?;
            // Refuse to clobber a stored edit that exists but no longer parses into the current schema
            // (corruption / breaking change). Overwriting it would permanently destroy the user's real
            // adjustments — the non-destructive guarantee. An explicit Reset passes `force=true` to
            // discard it deliberately; a stray slider commit (no force) cannot.
            if !force.unwrap_or(false) && stored_edit_is_unreadable(before.as_deref()) {
                return Err(format!(
                    "stored edit for image {image_id} is unreadable (schema mismatch or \
                 corruption); Reset to discard it"
                ));
            }
            // Commit the edit AND the durable intent to mirror it into the sidecar beside the RAW in
            // one transaction (migration 026): if the process dies before the file lands, the next
            // launch still knows this revision is owed a write. `set_edit` would only have moved the
            // params, leaving the "still owed" knowledge in memory.
            let revision = core_library::save_edit_with_intent(
                &db.conn,
                image_id,
                PROCESS_VERSION,
                &json,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())?;
            // Rating/keywords ride along in the same sidecar, so the coalescing queue keeps its mark;
            // the outbox worker is what guarantees *this* revision reaches disk.
            sync_sidecar(st.inner(), image_id);
            let _ = core_library::append_event(
                &db.conn,
                &crate::events::stamp(
                    st.inner(),
                    core_library::Event {
                        event_type: "develop.params_commit".into(),
                        image_id: Some(image_id),
                        process_version: Some(PROCESS_VERSION),
                        params_before: before,
                        params_after: Some(json),
                        touch_count,
                        ..Default::default()
                    },
                ),
            );
            // Wake the mirroring worker now that the intent is committed. Takes only the worker's own
            // lock, and only after the catalog guard is gone, so no lock order is introduced.
            drop(db);
            st.outbox.notify();
            // The committed revision: the quit barrier's ack is checked against it.
            Ok(revision)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// The frontend's answer to the quit barrier's `app:flush-edits` request. `barrier_id` echoes the
/// request's `barrierId`; `ok = false` means an edit is still unsaved, which cancels this quit
/// attempt; `saved` lists the committed revision per image, verified against the catalog (see
/// `crate::quit`). An ack for any other barrier is ignored.
#[tauri::command]
pub fn develop_flush_ack(
    app: AppHandle,
    barrier_id: u64,
    ok: bool,
    saved: Vec<crate::quit::SavedRevisionArg>,
) {
    crate::quit::deliver_ack(&app, barrier_id, ok, saved);
}

/// Run the save barrier for an updater relaunch. `Ok(true)` = safe to relaunch.
///
/// The updater restarts the process through `plugin-process`, which never raises a quit the barrier
/// can hold, so the frontend must ask before calling `relaunch()`. A rejected barrier leaves the app
/// running with `app:save-barrier` announced; the caller must not relaunch on anything but `true`.
///
/// Deliberately NOT an ordered `UserCommand` job: the frontend's flush is an ordered
/// `develop_set_edit`, which could not start until this barrier timed out. The coordinator runs on
/// its own worker thread and this command only awaits its result.
#[tauri::command]
pub async fn prepare_restart(app: AppHandle) -> Result<bool, String> {
    let reason = crate::quit::BarrierReason::UpdaterRestart;
    let result = crate::quit::run_save_barrier(&app, reason).await;
    if result.is_safe() {
        Ok(true)
    } else {
        Err(result.message(reason))
    }
}

/// Hard cap on the full-res texture's long edge — keeps it within the GPU max texture dimension
/// (8192 on the wgpu defaults). A no-op for the validated EOS R7 (6960 px).
pub(crate) const FULL_MAX_EDGE: u32 = 8192;

/// Output edge for the whole-crop histogram render. Small + square: a histogram is invariant to the
/// (anisotropic) resample, and `{0,0,1,1}` fills the whole output with the entire developed crop.
const HIST_RENDER_EDGE: u32 = 384;

fn profiling() -> bool {
    std::env::var_os("DARKROOM_PROFILE").is_some()
}

/// Demosaic-free instant first paint: the camera's embedded preview JPEG (no GPU, no decode of the
/// sensor data, no cache lock). The frontend shows this within ~tens of ms while `develop_render`
/// produces the color-managed, edit-applied result in the background.
#[tauri::command]
pub async fn develop_preview_jpeg(
    app: AppHandle,
    image_id: i64,
) -> Result<tauri::ipc::Response, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Viewport,
        false,
        "develop_preview_jpeg",
        move || {
            let st = app.state::<AppState>();
            let path = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::image_by_id(&db.conn, image_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "image not found".to_string())?
                    .path
            };
            let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
            let thumb = core_raw::thumbnail_jpeg(&src, 2560, 85).map_err(|e| e.to_string())?;
            Ok::<_, String>(tauri::ipc::Response::new(thumb.jpeg))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

const THUMB_EDIT_EDGE: u32 = 1024;
const THUMB_EDIT_QUALITY: u8 = 85;

/// A canonical develop render to JPEG: the image's content hash, the rendered bytes, and the edit
/// version (`updated_at`) when the image has a stored edit (`None` → rendered from defaults).
pub struct DevelopRender {
    pub hash: String,
    pub jpeg: Vec<u8>,
    /// `Some(updated_at)` when rendered from a stored edit; `None` when from `DevelopParams::default()`.
    pub edit_version: Option<i64>,
}

/// Whether `image_id`'s source is a display-referred image (JPEG/PNG) → the develop pipeline must
/// bypass the scene-referred base tone operator. Memoized per image so the hot `develop_render` path
/// (one call per slider move) reads the DB only when the open image changes.
pub(crate) fn image_display_referred(st: &AppState, image_id: i64) -> Result<bool, String> {
    let _source = st.publications.mutation_guard();
    {
        let memo = st.display_referred_memo.lock().map_err(|e| e.to_string())?;
        if let Some((id, v)) = *memo {
            if id == image_id {
                return Ok(v);
            }
        }
    }
    let path = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?
            .path
    };
    let v = core_raw::is_display(Path::new(&path));
    *st.display_referred_memo.lock().map_err(|e| e.to_string())? = Some((image_id, v));
    Ok(v)
}

/// Decode the full-resolution, oriented source using the stored edits. Canonical thumbnails and
/// previews use the same denoise source selection as export before deriving their smaller tiers.
pub(crate) fn decode_develop(
    st: &AppState,
    image_id: i64,
) -> Result<(String, DevelopParams, Option<i64>, core_raw::LinearImage), String> {
    let (path, hash) = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let img = core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?;
        (img.path, img.content_hash)
    };
    let edit = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::get_edit_with_version(&db.conn, image_id).map_err(|e| e.to_string())?
    };
    let (mut params, edit_version) = match edit {
        Some((json, version)) => (
            serde_json::from_str::<DevelopParams>(&json).map_err(|e| e.to_string())?,
            Some(version),
        ),
        None => (DevelopParams::default(), None),
    };
    // Display-referred images (JPEG/PNG) bypass the scene-referred base tone operator so an unedited
    // image renders as itself. Intrinsic to the file, so derive it here (never trust persisted edits).
    params.display_referred = core_raw::is_display(Path::new(&path));
    let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
    let lin = decode_develop_source(st, &src, &params)?;
    Ok((hash, params, edit_version, lin))
}

fn decode_develop_source(
    st: &AppState,
    src: &core_raw::RawSource,
    params: &DevelopParams,
) -> Result<core_raw::LinearImage, String> {
    if params.denoise.is_off() {
        core_raw::develop_linear(src).map_err(|e| e.to_string())
    } else {
        crate::denoise::denoised_full(st, src, params.denoise.amount)
    }
}

/// A failed SAM rebuild must not produce a successful export/cached preview with missing edits.
/// Validate only layers the GPU consumes; disabled masks and layers beyond MASK_CAP have no effect.
fn validate_ai_coverages(
    params: &DevelopParams,
    coverages: &[core_pipeline::AiCoverage],
) -> Result<(), String> {
    for mask in params
        .masks
        .iter()
        .filter(|m| m.enabled)
        .take(core_pipeline::MASK_CAP)
    {
        if let Some(key) = core_pipeline::mask::mask_ai_hash(mask) {
            let valid = coverages.iter().any(|c| {
                c.hash == key
                    && c.width > 0
                    && c.height > 0
                    && c.width <= core_pipeline::AI_TEX_EDGE
                    && c.height <= core_pipeline::AI_TEX_EDGE
                    && (c.width as usize).checked_mul(c.height as usize) == Some(c.alpha.len())
            });
            if !valid {
                return Err(format!(
                    "AI mask '{}' could not be restored; open Develop and rebuild the mask before rendering",
                    mask.name
                ));
            }
        }
    }
    Ok(())
}

pub(crate) fn required_ai_coverages(
    st: &AppState,
    image_id: i64,
    params: &DevelopParams,
) -> Result<Vec<core_pipeline::AiCoverage>, String> {
    let coverages = crate::segment::coverages_for(st, image_id, params);
    validate_ai_coverages(params, &coverages)?;
    Ok(coverages)
}

#[cfg(test)]
mod render_coverage_tests {
    use super::validate_ai_coverages;
    use core_pipeline::{AiCoverage, ComponentKind, DevelopParams, Mask, MaskComponent};

    fn masked_params() -> DevelopParams {
        DevelopParams {
            masks: vec![Mask {
                name: "subject".into(),
                components: vec![MaskComponent {
                    kind: ComponentKind::Ai {
                        model: "test".into(),
                        points: vec![],
                        hash: "subject".into(),
                    },
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn saved_active_ai_masks_cannot_silently_disappear_from_output() {
        let mut params = masked_params();
        assert!(validate_ai_coverages(&params, &[]).is_err());
        let mut coverage = AiCoverage {
            hash: "subject".into(),
            width: 2,
            height: 2,
            alpha: vec![0; 4],
        };
        assert!(
            validate_ai_coverages(&params, std::slice::from_ref(&coverage)).is_ok(),
            "legitimate all-zero coverage is still a restored mask"
        );
        coverage.alpha.pop();
        assert!(validate_ai_coverages(&params, &[coverage]).is_err());
        params.masks[0].enabled = false;
        assert!(validate_ai_coverages(&params, &[]).is_ok());
    }
}

/// Render the whole crop directly at its native dimensions, including quarter-turn axis swaps.
/// A source-sized letterbox followed by a pixel cut would upscale small crops and lose resolution
/// on 90-degree rotation. This uses the same crop-local geometry as Develop, with a transient
/// prepared source and the request's AI coverage; it never touches the interactive render cache.
pub(crate) fn render_linear_cropped(
    gpu: &crate::state::GpuRender,
    lin: &core_raw::LinearImage,
    params: &DevelopParams,
    ai_coverages: &[core_pipeline::AiCoverage],
) -> Result<(Vec<u8>, u32, u32), String> {
    validate_ai_coverages(params, ai_coverages)?;
    gpu.pipeline
        .render_cropped_with_ai_coverages(&gpu.ctx, lin, params, ai_coverages)
        .map_err(|e| e.to_string())
}

/// Preserve the requested crop's detail when budgeting the source upload, then cap the rendered
/// output edge. Resizing only the uncropped source to the requested edge makes tight crops blurry.
pub(crate) fn render_linear_preview(
    gpu: &crate::state::GpuRender,
    lin: core_raw::LinearImage,
    params: &DevelopParams,
    ai_coverages: &[core_pipeline::AiCoverage],
    max_edge: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let max_edge = max_edge.max(1);
    let source_edge =
        params
            .crop
            .preview_source_edge(lin.width, lin.height, max_edge, gpu.ctx.max_texture_dim);
    let lin = lin.downscale_into_hq(source_edge);
    let (rgba, w, h) = render_linear_cropped(gpu, &lin, params, ai_coverages)?;
    // Integer source dimensions and crop rounding can overshoot by a pixel; never upscale when
    // the native crop or the device cap limits how many pixels are available.
    if w.max(h) > max_edge {
        Ok(core_pipeline::resize_rgba8(&rgba, w, h, max_edge))
    } else {
        Ok((rgba, w, h))
    }
}

/// Render an image through the canonical develop pipeline to a crop-applied JPEG at `max_edge`
/// (longest edge). Decodes (full demosaic), downscales (Lanczos3), renders + crops. A transient
/// `PreparedImage` — never the interactive `full_render_cache`.
pub fn render_develop_jpeg(
    st: &AppState,
    gpu: &crate::state::GpuRender,
    image_id: i64,
    max_edge: u32,
    quality: u8,
) -> Result<DevelopRender, String> {
    let (hash, params, edit_version, lin) = decode_develop(st, image_id)?;
    let coverages = required_ai_coverages(st, image_id, &params)?;
    let (rgba, w, h) = render_linear_preview(gpu, lin, &params, &coverages, max_edge)?;
    let jpeg = core_pipeline::rgba8_to_jpeg(&rgba, w, h, quality).map_err(|e| e.to_string())?;
    Ok(DevelopRender {
        hash,
        jpeg,
        edit_version,
    })
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct EditChanged {
    image_id: i64,
    /// Edit version (`updated_at`) for cache-busting previews, or null when the edit was cleared.
    edited_at: Option<i64>,
}

/// Regenerate the edited thumbnail for an image and notify the frontend so the grid/filmstrip swap
/// to the new versioned `thumb://` URL. Called on edit-settle (after persist). Returns the current
/// edit version, or null when the image has no edit; a superseded render never publishes old state.
#[tauri::command]
pub async fn develop_regen_thumb(app: AppHandle, image_id: i64) -> Result<Option<i64>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Thumbnail,
        false,
        "develop_regen_thumb",
        move || {
            let st = app.state::<AppState>();
            let gpu = st
                .gpu
                .as_ref()
                .ok_or_else(|| "GPU develop unavailable".to_string())?;
            let sequence = st.publications.sequence();
            let source = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::image_by_id(&db.conn, image_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "image not found".to_string())?
            };
            let token = st
                .publications
                .capture_at(
                    core_model::identity::ImageId(image_id),
                    core_runtime::publication::SourceKey {
                        path: source.path,
                        hash: source.content_hash,
                    },
                    sequence,
                )
                .map_err(|_| "thumbnail source changed".to_string())?;
            let render = render_develop_jpeg(
                st.inner(),
                gpu,
                image_id,
                THUMB_EDIT_EDGE,
                THUMB_EDIT_QUALITY,
            )?;
            let edited_at = st
                .publications
                .with_current(&token, || -> Result<Option<i64>, String> {
                    let current_version = {
                        let db = st.db.lock().map_err(|e| e.to_string())?;
                        core_library::get_edit_with_version(&db.conn, image_id)
                            .map_err(|e| e.to_string())?
                            .map(|(_, version)| version)
                    };
                    if current_version != render.edit_version {
                        return Ok(current_version);
                    }
                    match render.edit_version {
                        Some(version) => st.thumbs.write_edited(
                            &render.hash,
                            PROCESS_VERSION,
                            version,
                            &render.jpeg,
                        ),
                        None => {
                            st.thumbs
                                .write_canonical(&render.hash, PROCESS_VERSION, &render.jpeg)
                        }
                    }
                    .map_err(|e| e.to_string())?;
                    let _ = app.emit(
                        "develop:edit-changed",
                        EditChanged {
                            image_id,
                            edited_at: current_version,
                        },
                    );
                    Ok(current_version)
                })
                .map_err(|_| "thumbnail source changed".to_string())??;
            crate::thumb_queue::enforce_thumb_cap_after_write(st.inner());
            Ok(edited_at)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Promote image ids to the front of the canonical-thumbnail backfill queue (the visible grid range
/// and the image about to open in Develop), so they render before the bulk backfill. Cheap; runs
/// synchronously on the main thread (just a queue splice).
#[tauri::command]
pub fn thumb_prioritize(app: AppHandle, image_ids: Vec<i64>) {
    app.state::<AppState>().thumb_queue.prioritize(&image_ids);
}

/// Mark whether a Develop editing session is active. While active, the background thumbnail worker
/// parks between jobs so interactive renders always win the next GPU slot.
#[tauri::command]
pub fn develop_session(app: AppHandle, active: bool) {
    app.state::<AppState>().thumb_queue.set_interactive(active);
}

/// The histogram of the most recent successful render. A reliable pull-fallback for the
/// fire-and-forget `develop:histogram` event (which can be missed if it fires before the listener
/// is registered). Returns `None` if nothing has rendered yet.
#[tauri::command]
pub async fn develop_get_histogram(app: AppHandle) -> Result<Option<Histogram>, String> {
    let st = app.state::<AppState>();
    let hist = st.last_histogram.lock().map_err(|e| e.to_string())?.clone();
    Ok(hist)
}

/// A crop-local uv viewport window: origin `(ox, oy)` + size `(sx, sy)`, all in [0,1] crop space.
#[derive(serde::Deserialize)]
pub struct ViewRect {
    ox: f32,
    oy: f32,
    sx: f32,
    sy: f32,
}

/// Resolve a frontend mask *index* (its position in `params.masks`) to the dense GPU overlay
/// *layer* the mask pre-pass packs into. The pre-pass only uploads enabled masks, so disabled
/// masks before `idx` shift every later mask down a layer. Returns -1 (no overlay) when `idx` is
/// negative, out of range, the targeted mask is disabled, or its packed layer would exceed the GPU
/// mask cap.
fn packed_overlay_layer(params: &DevelopParams, idx: i32) -> i32 {
    if idx < 0 {
        return -1;
    }
    let i = idx as usize;
    match params.masks.get(i) {
        Some(m) if m.enabled => {
            let packed = params.masks[..i].iter().filter(|m| m.enabled).count();
            if packed >= core_pipeline::MASK_CAP {
                -1
            } else {
                packed as i32
            }
        }
        _ => -1,
    }
}

/// Outcome of claiming the decode slot for one `(image_id, tier)`.
pub(crate) enum ClaimOutcome<'a> {
    /// The cache became warm (another thread decoded it) — nothing to do.
    Warm,
    /// A newer request superseded this one while waiting.
    Superseded,
    /// This thread owns the decode; the guard releases the claim (and wakes waiters) on drop.
    Decode(DecodeClaim<'a>),
}

/// RAII claim on `decode_inflight` — removed + waiters notified on drop (any exit path).
pub(crate) struct DecodeClaim<'a> {
    st: &'a AppState,
    key: (i64, bool),
}

impl Drop for DecodeClaim<'_> {
    fn drop(&mut self) {
        if let Ok(mut s) = self.st.decode_inflight.lock() {
            s.remove(&self.key);
        }
        self.st.decode_cv.notify_all();
    }
}

/// Claim the decode for `key`, waiting while another thread decodes the same tier (so a slider
/// burst / histogram right after open never duplicates the multi-second RAW decode). `present`
/// re-checks the tier cache each wake.
pub(crate) fn claim_decode<'a>(
    st: &'a AppState,
    key: (i64, bool),
    present: impl Fn() -> Result<bool, String>,
    superseded: &impl Fn() -> bool,
) -> Result<ClaimOutcome<'a>, String> {
    let mut inflight = st.decode_inflight.lock().map_err(|e| e.to_string())?;
    loop {
        if present()? {
            return Ok(ClaimOutcome::Warm);
        }
        if superseded() {
            return Ok(ClaimOutcome::Superseded);
        }
        if !inflight.contains(&key) {
            inflight.insert(key);
            return Ok(ClaimOutcome::Decode(DecodeClaim { st, key }));
        }
        // Timed wait: robust against a missed notify; the loop re-checks cache + supersede.
        let (guard, _) = st
            .decode_cv
            .wait_timeout(inflight, std::time::Duration::from_millis(100))
            .map_err(|e| e.to_string())?;
        inflight = guard;
    }
}

/// Ensure the full-resolution decoded + GPU-prepared source for `image_id` lives in
/// `full_render_cache` (keyed by id). Evicts a different image's texture first, then decodes (lock
/// released across the expensive decode) + prepares only when absent. Returns `false` WITHOUT
/// populating when `superseded()` turns true before the decode (caller should bail with an empty
/// response); `true` once the cache holds `image_id`.
fn ensure_full_render_cache(
    st: &AppState,
    gpu: &GpuRender,
    image_id: i64,
    superseded: impl Fn() -> bool,
) -> Result<bool, String> {
    let sequence = st.publications.sequence();
    let source = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?
    };
    let token = st
        .publications
        .capture_at(
            core_model::identity::ImageId(image_id),
            core_runtime::publication::SourceKey {
                path: source.path.clone(),
                hash: source.content_hash,
            },
            sequence,
        )
        .map_err(|_| "source changed before decode".to_string())?;
    // Free the large full-res texture as soon as we render a different image.
    {
        let mut slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _)| *id != image_id) {
            *slot = None;
        }
    }
    {
        let mut slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _)| *id != image_id) {
            *slot = None;
        }
    }

    // --- Full-res source: decode the whole frame at full demosaic, cached in a single slot. ---
    let present = || -> Result<bool, String> {
        let slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
        Ok(slot.as_ref().is_some_and(|(id, _)| *id == image_id))
    };
    let _claim = match claim_decode(st, (image_id, false), present, &superseded)? {
        ClaimOutcome::Warm => return Ok(true),
        ClaimOutcome::Superseded => return Ok(false),
        ClaimOutcome::Decode(claim) => claim,
    };
    {
        let path = source.path;
        let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
        let t = Instant::now();
        let lin = core_raw::develop_linear(&src)
            .map_err(|e| e.to_string())?
            .downscale_into(FULL_MAX_EDGE);
        if profiling() {
            tracing::debug!(elapsed_ms = t.elapsed().as_millis(), "decode full profile");
        }
        // Re-check AFTER the slow decode: if the user switched images meanwhile, DON'T store — a
        // late store would clobber the newer image's prepared texture and force a re-decode.
        // (Keyed on the image, not the request seq: slider moves on the same image must keep it.)
        if st.current_image.load(Ordering::SeqCst) != image_id {
            return Ok(false);
        }
        let prepared = gpu
            .pipeline
            .prepare(&gpu.ctx, &lin)
            .map_err(|e| e.to_string())?;
        st.publications
            .with_current(&token, || -> Result<bool, String> {
                if superseded() || st.workers.is_stopping() {
                    return Ok(false);
                }
                *st.full_render_cache.lock().map_err(|e| e.to_string())? =
                    Some((image_id, std::sync::Arc::new(prepared)));
                Ok(true)
            })
            .unwrap_or(Ok(false))
    }
}

fn ensure_preview_render_cache(
    st: &AppState,
    gpu: &GpuRender,
    image_id: i64,
    superseded: impl Fn() -> bool,
) -> Result<bool, String> {
    let sequence = st.publications.sequence();
    let source = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::image_by_id(&db.conn, image_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "image not found".to_string())?
    };
    let token = st
        .publications
        .capture_at(
            core_model::identity::ImageId(image_id),
            core_runtime::publication::SourceKey {
                path: source.path.clone(),
                hash: source.content_hash,
            },
            sequence,
        )
        .map_err(|_| "source changed before decode".to_string())?;
    {
        let mut slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_some_and(|(id, _)| *id != image_id) {
            *slot = None;
        }
    }
    let present = || -> Result<bool, String> {
        let slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
        Ok(slot.as_ref().is_some_and(|(id, _)| *id == image_id))
    };
    let _claim = match claim_decode(st, (image_id, true), present, &superseded)? {
        ClaimOutcome::Warm => return Ok(true),
        ClaimOutcome::Superseded => return Ok(false),
        ClaimOutcome::Decode(claim) => claim,
    };
    {
        // Warm-path: a prefetched (or previously decoded) linear preview skips the decode entirely
        // — switching to a predicted neighbor is a GPU upload (~100 ms), not a multi-second decode.
        let cached = st
            .preview_linear_lru
            .lock()
            .map_err(|e| e.to_string())?
            .get(image_id);
        let lin = match cached {
            Some(lin) => lin,
            None => {
                let path = source.path;
                let src =
                    core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
                let t = Instant::now();
                let lin = std::sync::Arc::new(
                    core_raw::develop_linear_preview(&src).map_err(|e| e.to_string())?,
                );
                if profiling() {
                    tracing::debug!(
                        elapsed_ms = t.elapsed().as_millis(),
                        "decode preview profile"
                    );
                }
                // Cache the decode so navigating BACK to this image is instant.
                let _ = st.publications.with_current(&token, || {
                    st.preview_linear_lru
                        .lock()
                        .map_err(|e| e.to_string())?
                        .insert(image_id, lin.clone());
                    Ok::<_, String>(())
                });
                lin
            }
        };
        // Same late-store guard as the full cache (rapid image switching must not clobber).
        if st.current_image.load(Ordering::SeqCst) != image_id {
            return Ok(false);
        }
        let prepared = gpu
            .pipeline
            .prepare(&gpu.ctx, &lin)
            .map_err(|e| e.to_string())?;
        st.publications
            .with_current(&token, || -> Result<bool, String> {
                if superseded() || st.workers.is_stopping() {
                    return Ok(false);
                }
                *st.preview_render_cache.lock().map_err(|e| e.to_string())? =
                    Some((image_id, std::sync::Arc::new(prepared)));
                Ok(true)
            })
            .unwrap_or(Ok(false))
    }
}

/// Predictively decode the half-res previews of `image_ids` (the frontend's next/prev neighbors)
/// into the CPU linear LRU, so stepping through a collection hits a warm buffer (GPU upload only,
/// ~100 ms) instead of a multi-second decode. CPU-only — never touches the GPU queue. A newer call
/// REPLACES the previous set rather than adding to it, and one worker runs them, so holding `next`
/// can never leave several speculative decodes competing with the image the user is looking at.
#[tauri::command]
pub async fn develop_prefetch(app: AppHandle, image_ids: Vec<i64>) -> Result<(), String> {
    let st = app.state::<AppState>();
    let my_gen = st.prefetch_gen.fetch_add(1, Ordering::SeqCst) + 1;
    st.prefetch_queue.request(my_gen, image_ids);
    Ok(())
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DevelopFullReady {
    image_id: i64,
}

fn warm_full_render_cache(app: AppHandle, image_id: i64) {
    let host = app.clone();
    crate::jobs::background(&host, core_app::jobs::JobClass::Prefetch, move || {
        let st = app.state::<AppState>();
        let Some(gpu) = st.gpu.as_ref() else { return };
        let already_full = st
            .full_render_cache
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|(id, _)| *id == image_id))
            .unwrap_or(false);
        if already_full {
            return;
        }
        // Abort once the user has switched images: a late full-res store for the OLD image would
        // clobber the new image's prepared texture and force a re-decode. Slider moves on the same
        // image keep `current_image` stable, so the warm still completes for them.
        let superseded = || st.current_image.load(Ordering::SeqCst) != image_id;
        if matches!(
            ensure_full_render_cache(st.inner(), gpu, image_id, superseded),
            Ok(true)
        ) {
            let _ = app.emit("develop:full-ready", DevelopFullReady { image_id });
        }
    });
}

fn can_use_preview_source(view: &core_pipeline::ViewParams) -> bool {
    // The preview source is HALF resolution: any view showing ≥ ~45% of the frame per axis maps at
    // ≤ ~2× minification on it, where the half-res source both looks correct and ALIASES LESS than
    // an undersampled full-res source (the shader has no mips). Tighter zooms need full res.
    view.size[0] >= 0.45 && view.size[1] >= 0.45 && view.overlay_layer < 0
}

/// Render a crop-local viewport window of `image_id` at the requested output size, returning raw
/// RGBA8 framed by its dimensions: `[out_w u32 LE][out_h u32 LE][rgba8 (out_w*out_h*4)]`.
///
/// The prepared SOURCE is always full-resolution (decoded once and cached in `full_render_cache`,
/// keyed by `image_id`) so any zoom stays crisp; only the small `out_w × out_h` viewport target is
/// re-rendered per edit (the RapidRAW pattern). The expensive decode runs WITHOUT holding the
/// cache lock. `request_id` is a monotonic supersede token: if a newer request has overtaken this
/// one, the decode is skipped and an empty response is returned. (Instant first-paint is served
/// separately by `develop_preview_jpeg`.)
#[allow(clippy::too_many_arguments)] // fixed by the viewport-render IPC contract
#[tauri::command]
pub async fn develop_render(
    app: AppHandle,
    image_id: i64,
    mut params: DevelopParams,
    view: ViewRect,
    out_w: u32,
    out_h: u32,
    overlay_mask_index: i32,
    crop_preview: bool,
    request_id: u64,
) -> Result<tauri::ipc::Response, String> {
    // Client counters restart on view/webview reload; the supervisor assigns ordered host IDs.
    let _client_request_id = request_id;
    crate::jobs::viewport_render(app.clone(), image_id, move |request_id| {
        let prof = profiling();
        let st = app.state::<AppState>();
        let gpu = st
            .gpu
            .as_ref()
            .ok_or_else(|| "GPU develop unavailable".to_string())?;

        // Authoritative (intrinsic to the file): never trust the FE-sent value.
        params.display_referred = image_display_referred(st.inner(), image_id)?;

        let superseded = || st.latest_render.load(Ordering::SeqCst) > request_id;

        // Clamp to the device limit HERE so the response header always matches the rendered
        // payload (render_view clamps internally with the same bound; a mismatch would misframe
        // the pixel data and blank the canvas).
        let max_edge = gpu.ctx.max_texture_dim;
        let out_w = out_w.clamp(1, max_edge);
        let out_h = out_h.clamp(1, max_edge);

        // Build the viewport descriptor and render the crop-local window into an out_w × out_h
        // target. The cache lock is only held to clone the Arc handle; per-image serialization
        // lives inside PreparedImage::render_lock.
        let view = core_pipeline::ViewParams {
            origin: [view.ox, view.oy],
            size: [view.sx, view.sy],
            out_w,
            out_h,
            active: true,
            overlay_layer: packed_overlay_layer(&params, overlay_mask_index),
            overlay_color: [0.85, 0.10, 0.10],
            overlay_strength: 0.5,
            crop_preview,
        };

        let have_full = {
            let slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
            slot.as_ref().is_some_and(|(id, _)| *id == image_id)
        };
        let use_preview = !have_full && can_use_preview_source(&view);
        if use_preview {
            if !ensure_preview_render_cache(st.inner(), gpu, image_id, superseded)? {
                return Ok(tauri::ipc::Response::new(Vec::new()));
            }
        } else if !ensure_full_render_cache(st.inner(), gpu, image_id, superseded)? {
            // A newer request overtook this one before the (skipped) decode.
            return Ok(tauri::ipc::Response::new(Vec::new()));
        }

        let t = Instant::now();
        // Clone the Arc handle and DROP the cache lock before rendering: the slot stays swappable
        // while the GPU works (rapid image switches never queue behind a readback stall), and the
        // id filter guarantees we never render a different image that raced into the slot.
        let prepared = if use_preview {
            let slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
            slot.as_ref()
                .filter(|(id, _)| *id == image_id)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| "preview image evicted before render".to_string())?
        } else {
            let slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
            slot.as_ref()
                .filter(|(id, _)| *id == image_id)
                .map(|(_, p)| p.clone())
                .ok_or_else(|| "full-res image evicted before render".to_string())?
        };
        // Resolve AI (SAM) coverage for this request so another viewport/histogram cannot replace
        // it before the GPU mask pre-pass. Missing saved coverage is an error, never an empty mask.
        let ai_covs = required_ai_coverages(st.inner(), image_id, &params)?;
        let rgba = gpu
            .pipeline
            .render_view_with_ai_coverages(&gpu.ctx, &prepared, &params, &view, &ai_covs)
            .map_err(|e| e.to_string())?;
        if prof {
            tracing::debug!(elapsed_ms = t.elapsed().as_millis(), "gpu render profile");
        }
        if use_preview {
            warm_full_render_cache(app.clone(), image_id);
        }

        // The histogram is computed by the dedicated `develop_histogram` whole-crop pass (driven on
        // param change), NOT from this viewport buffer — so it stays correct (whole image) while
        // zoomed and is not recomputed on every pan/zoom.

        // Frame the raw RGBA8 with its dimensions and source tier:
        // [out_w LE][out_h LE][flags u8: bit0=preview-source][rgba8].
        // Never frame a payload whose length disagrees with the header — the frontend would
        // construct ImageData from misaligned bytes.
        if rgba.len() != (out_w as usize) * (out_h as usize) * 4 {
            return Err(format!(
                "render size mismatch: got {} bytes, expected {}x{}x4",
                rgba.len(),
                out_w,
                out_h
            ));
        }
        let mut buf = Vec::with_capacity(9 + rgba.len());
        buf.extend_from_slice(&out_w.to_le_bytes());
        buf.extend_from_slice(&out_h.to_le_bytes());
        buf.push(u8::from(use_preview));
        buf.extend_from_slice(&rgba);
        Ok::<_, String>(tauri::ipc::Response::new(buf))
    })
    .await
}

/// Compute the histogram of the WHOLE developed crop (not the visible viewport). Renders the full
/// crop-local frame `{0,0,1,1}` at a small fixed size and histograms it, so the Develop panel
/// reflects the entire image regardless of pan/zoom. Stored for pull (`develop_get_histogram`) +
/// emitted on `develop:histogram` for push. Driven by the frontend on param / before-after change
/// (NOT on pan/zoom), so the extra small render is cheap and infrequent.
#[tauri::command]
pub async fn develop_histogram(
    app: AppHandle,
    image_id: i64,
    mut params: DevelopParams,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Viewport,
        false,
        "develop_histogram",
        move || {
            let st = app.state::<AppState>();
            let sequence = st.publications.sequence();
            let gpu = st
                .gpu
                .as_ref()
                .ok_or_else(|| "GPU develop unavailable".to_string())?;
            params.display_referred = image_display_referred(st.inner(), image_id)?;

            let view = core_pipeline::ViewParams {
                origin: [0.0, 0.0],
                size: [1.0, 1.0],
                out_w: HIST_RENDER_EDGE,
                out_h: HIST_RENDER_EDGE,
                active: true,
                overlay_layer: -1,
                overlay_color: [0.0, 0.0, 0.0],
                overlay_strength: 0.0,
                crop_preview: false,
            };
            // Order guard: two overlapping histogram calls (rapid slider + before/after) can finish out
            // of order; only the newest may store/emit.
            let seq = st.hist_seq.fetch_add(1, Ordering::SeqCst) + 1;

            // Reuse a WARM cache only — never decode here. Prefer the full-res slot; fall back to the
            // half-res preview slot (at HIST_RENDER_EDGE the tiers are indistinguishable), so the panel
            // has data while the full decode is still in flight. Skip only when BOTH are cold — the
            // frontend re-triggers after the first viewport render warms a cache.
            let prepared = {
                let slot = st.full_render_cache.lock().map_err(|e| e.to_string())?;
                slot.as_ref()
                    .filter(|(id, _)| *id == image_id)
                    .map(|(_, p)| p.clone())
            };
            let prepared = match prepared {
                Some(p) => p,
                None => {
                    let slot = st.preview_render_cache.lock().map_err(|e| e.to_string())?;
                    match slot
                        .as_ref()
                        .filter(|(id, _)| *id == image_id)
                        .map(|(_, p)| p.clone())
                    {
                        Some(p) => p,
                        None => return Ok(()),
                    }
                }
            };
            // Resolve coverage for THIS histogram's params, not whichever viewport rendered last.
            let ai_covs = required_ai_coverages(st.inner(), image_id, &params)?;
            // Cache locks dropped — the render serializes only on the per-image render_lock.
            let rgba = gpu
                .pipeline
                .render_view_with_ai_coverages(&gpu.ctx, &prepared, &params, &view, &ai_covs)
                .map_err(|e| e.to_string())?;

            if st.hist_seq.load(Ordering::SeqCst) != seq {
                return Ok(()); // superseded by a newer histogram request
            }
            let hist = core_pipeline::histogram(&rgba);
            let _ = st.publications.with_sequence(sequence, || {
                if let Ok(mut last) = st.last_histogram.lock() {
                    *last = Some(hist.clone());
                }
                let _ = app.emit("develop:histogram", hist);
            });
            Ok::<_, String>(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Export ----------

/// Write `bytes` to `dest` atomically: a `.part` sibling first, then rename into place.
///
/// A half-written export is indistinguishable from a finished one — to the user browsing the
/// folder, and to any tool that opens it (a truncated JPEG still decodes to a partial image). The
/// rename is atomic within a filesystem, so the destination either does not exist or is complete.
/// A unique temporary file isolates concurrent exports; flushing and syncing must succeed before
/// replacement. An explicit export may replace a previous export, but never a catalog source.
fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic_with_policy(dest, bytes, true)
}

/// Batch photo export has no per-file overwrite confirmation; collisions must preserve both the
/// existing photograph and any concurrent writer's output. Preset save uses explicit replacement.
fn write_new_export(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    write_atomic_with_policy(dest, bytes, false)
}

fn write_atomic_with_policy(dest: &Path, bytes: &[u8], replace: bool) -> Result<(), String> {
    use std::io::Write;
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut part = tempfile::Builder::new()
        .prefix(".darkroom-export-")
        .suffix(".part")
        .tempfile_in(parent)
        .map_err(|e| e.to_string())?;
    part.write_all(bytes).map_err(|e| e.to_string())?;
    part.as_file().sync_all().map_err(|e| e.to_string())?;
    (if replace {
        part.persist(dest)
    } else {
        part.persist_noclobber(dest)
    })
    .map(|_| ())
    .map_err(|e| e.error.to_string())
}

/// Protect every catalog source, including another image or a symlink alias, before publishing an
/// export. The DB lock covers only path lookup; filesystem resolution happens after it is released.
fn ensure_export_destination(st: &AppState, dest: &Path) -> Result<(), String> {
    let sources: Vec<String> = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        let mut stmt = db
            .conn
            .prepare("SELECT path FROM images")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };
    ensure_not_source(dest, &sources)
}

fn ensure_not_source(dest: &Path, sources: &[String]) -> Result<(), String> {
    // A missing original still owns its catalog path. Resolve its parent so aliases and `..`
    // cannot publish unrelated pixels under a missing row's old content identity.
    let resolve = |path: &Path| {
        path.canonicalize().ok().or_else(|| {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            Some(parent.canonicalize().ok()?.join(path.file_name()?))
        })
    };
    let resolved = resolve(dest);
    if sources.iter().any(|source| {
        let source = Path::new(source);
        source == dest
            || resolved
                .as_ref()
                .is_some_and(|dest| resolve(source).as_ref() == Some(dest))
    }) {
        return Err(
            "export destination is a catalog source image; choose a different filename".into(),
        );
    }
    Ok(())
}

/// Export a single image at full resolution through the develop pipeline to `dest`.
/// `format` is "png" or "jpeg". Originals are never modified.
#[tauri::command]
pub async fn export_image(
    app: AppHandle,
    image_id: i64,
    mut params: DevelopParams,
    format: String,
    dest: String,
    quality: Option<u8>,
) -> Result<(), String> {
    // JPEG encoder quality (1..=100); default 92 when unset. Ignored for PNG.
    let quality = quality.unwrap_or(92).clamp(1, 100);
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Export,
        true,
        "export_image",
        move || {
            let st = app.state::<AppState>();
            ensure_export_destination(st.inner(), Path::new(&dest))?;
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
            params.display_referred = core_raw::is_display(Path::new(&path));

            let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
            // When denoise is enabled for this image, export from the full-res denoised source (raw-domain
            // denoise + amount blend) instead of the plain develop.
            let lin = decode_develop_source(st.inner(), &src, &params)?;
            // Full-res export needs a source-sized GPU texture — fail with a clear message instead of
            // an opaque pipeline error. (Tiled/downscaled export for oversized sources is a follow-up.)
            let max_edge = gpu.ctx.max_texture_dim;
            if lin.width > max_edge || lin.height > max_edge {
                return Err(format!(
                    "image is {}×{} but this GPU supports at most {max_edge}px per side for export",
                    lin.width, lin.height
                ));
            }
            let ai_covs = required_ai_coverages(st.inner(), image_id, &params)?;
            let (rgba, cw, ch) = render_linear_cropped(gpu, &lin, &params, &ai_covs)?;

            // Border / frame composite (solid color + optional pad-to-aspect). A CPU post-step outside
            // the GPU pipeline; no-op unless the border is active.
            let (rgba, ow, oh) = if params.border.is_active() {
                core_pipeline::frame_rgba8(
                    &rgba,
                    cw,
                    ch,
                    params.border.color_rgba8(),
                    params.border.size,
                    params.border.aspect(),
                )
            } else {
                (rgba, cw, ch)
            };

            let bytes = match format.to_lowercase().as_str() {
                "png" => core_pipeline::rgba8_to_png(&rgba, ow, oh),
                "jpeg" | "jpg" => core_pipeline::rgba8_to_jpeg(&rgba, ow, oh, quality),
                other => return Err(format!("unsupported export format: {other}")),
            }
            .map_err(|e| e.to_string())?;

            ensure_export_destination(st.inner(), Path::new(&dest))?;
            write_new_export(Path::new(&dest), &bytes)?;
            // Export is the strongest edit-quality endorsement — log it (best-effort, separate lock).
            crate::events::log_event(
                st.inner(),
                core_library::Event {
                    event_type: "develop.export".into(),
                    image_id: Some(image_id),
                    process_version: Some(PROCESS_VERSION),
                    params_after: serde_json::to_string(&params).ok(),
                    context: Some(
                        serde_json::json!({ "format": format, "quality": quality }).to_string(),
                    ),
                    ..Default::default()
                },
            );
            Ok::<_, String>(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Culling ----------

async fn db_write<F>(app: AppHandle, f: F) -> Result<(), String>
where
    F: FnOnce(&core_db::rusqlite::Connection) -> Result<(), core_library::LibError>
        + Send
        + 'static,
{
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "db_write",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            f(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Mark an image's sidecar stale after a catalog mutation. The coalescing worker
/// (`sidecar_queue`) gathers and writes it with the DB lock released — writing inline here would
/// hold the single SQLite connection across an SSD write on every edit, and edits now land during
/// a slider drag. Sidecars stay the durable record of intent; the catalog is the live state.
fn sync_sidecar(st: &AppState, image_id: i64) {
    st.sidecar_queue.mark_dirty(image_id);
}

fn sync_sidecars(st: &AppState, image_ids: &[i64]) {
    st.sidecar_queue.mark_dirty_many(image_ids);
}

/// `flag` value → event-type label (for the behavioral log).
fn flag_event_type(flag: &str) -> &'static str {
    match flag {
        "pick" => "culling.flag_pick",
        "reject" => "culling.flag_reject",
        _ => "culling.flag_clear",
    }
}

/// What the suggestion badge said when the user acted on THIS image — the provenance half of a cull
/// event. All three fields are absent on a photo with no badge (and on every batch path, which is
/// deliberately classified as bulk keyboard work rather than judgement).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionCtx {
    /// Non-null marks the label as *prompted*; the value is only ever read by a human.
    pub suggester_id: Option<String>,
    pub suggestion_score: Option<f64>,
    /// "pick" | "reject" — the badge that was on screen.
    pub suggested_flag: Option<String>,
}

impl SuggestionCtx {
    /// Fold this context into an event: suggester + score onto their own columns, the badge into
    /// `context.suggested` (merged, so an existing context survives).
    fn apply(self, e: &mut core_library::Event) {
        e.suggester_id = self.suggester_id;
        e.suggestion_score = self.suggestion_score;
        e.context =
            core_library::context_with_suggested(e.context.take(), self.suggested_flag.as_deref());
    }
}

#[tauri::command]
pub async fn cull_set_rating(
    app: AppHandle,
    image_id: i64,
    stars: i64,
    latency_ms: Option<i64>,
    group_id: Option<String>,
    candidate_ids: Option<Vec<i64>>,
    suggestion: Option<SuggestionCtx>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_set_rating",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_rating(&db.conn, image_id, stars).map_err(|e| e.to_string())?;
            sync_sidecar(st.inner(), image_id);
            let mut event = core_library::Event {
                event_type: "culling.rate".into(),
                image_id: Some(image_id),
                stars: Some(stars),
                group_id,
                candidate_ids: candidate_ids.as_deref().map(core_library::ids_json),
                latency_ms,
                ..Default::default()
            };
            suggestion.unwrap_or_default().apply(&mut event);
            let _ = core_library::append_event(&db.conn, &crate::events::stamp(st.inner(), event));
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_flag(
    app: AppHandle,
    image_id: i64,
    flag: String,
    latency_ms: Option<i64>,
    group_id: Option<String>,
    candidate_ids: Option<Vec<i64>>,
    suggestion: Option<SuggestionCtx>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_set_flag",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_flag(&db.conn, image_id, &flag).map_err(|e| e.to_string())?;
            sync_sidecar(st.inner(), image_id);
            let mut event = core_library::Event {
                event_type: flag_event_type(&flag).into(),
                image_id: Some(image_id),
                chosen_id: (flag == "pick").then_some(image_id),
                flag: Some(flag),
                group_id,
                candidate_ids: candidate_ids.as_deref().map(core_library::ids_json),
                latency_ms,
                ..Default::default()
            };
            suggestion.unwrap_or_default().apply(&mut event);
            let _ = core_library::append_event(&db.conn, &crate::events::stamp(st.inner(), event));
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_label(
    app: AppHandle,
    image_id: i64,
    label: Option<String>,
    latency_ms: Option<i64>,
    group_id: Option<String>,
    suggestion: Option<SuggestionCtx>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_set_label",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_label(&db.conn, image_id, label.as_deref())
                .map_err(|e| e.to_string())?;
            sync_sidecar(st.inner(), image_id);
            let mut event = core_library::Event {
                event_type: "culling.label".into(),
                image_id: Some(image_id),
                color_label: label,
                group_id,
                latency_ms,
                ..Default::default()
            };
            suggestion.unwrap_or_default().apply(&mut event);
            let _ = core_library::append_event(&db.conn, &crate::events::stamp(st.inner(), event));
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// Batch culling — applies one value to a whole selection in a single transaction. The selection IS
// the candidate group, so one event per image carries the shared `group_id` + candidate set.

/// Append one `culling.*` event per image in a batch (same tx). `build` produces the per-image event.
fn log_batch(
    st: &AppState,
    conn: &core_db::rusqlite::Connection,
    image_ids: &[i64],
    group_id: &Option<String>,
    build: impl Fn(i64) -> core_library::Event,
) {
    let cands = core_library::ids_json(image_ids);
    for &id in image_ids {
        let mut e = build(id);
        e.image_id = Some(id);
        e.group_id = group_id.clone();
        e.candidate_ids = Some(cands.clone());
        let _ = core_library::append_event(conn, &crate::events::stamp(st, e));
    }
}

#[tauri::command]
pub async fn cull_set_rating_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    stars: i64,
    group_id: Option<String>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_set_rating_many",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_rating_many(&mut db.conn, &image_ids, stars)
                .map_err(|e| e.to_string())?;
            sync_sidecars(st.inner(), &image_ids);
            log_batch(st.inner(), &db.conn, &image_ids, &group_id, |_| {
                core_library::Event {
                    event_type: "culling.rate".into(),
                    stars: Some(stars),
                    ..Default::default()
                }
            });
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_flag_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    flag: String,
    group_id: Option<String>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_set_flag_many",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_flag_many(&mut db.conn, &image_ids, &flag)
                .map_err(|e| e.to_string())?;
            sync_sidecars(st.inner(), &image_ids);
            let et = flag_event_type(&flag);
            log_batch(st.inner(), &db.conn, &image_ids, &group_id, |id| {
                core_library::Event {
                    event_type: et.into(),
                    chosen_id: (flag == "pick").then_some(id),
                    flag: Some(flag.clone()),
                    ..Default::default()
                }
            });
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn cull_set_label_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    label: Option<String>,
    group_id: Option<String>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_set_label_many",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_label_many(&mut db.conn, &image_ids, label.as_deref())
                .map_err(|e| e.to_string())?;
            sync_sidecars(st.inner(), &image_ids);
            log_batch(st.inner(), &db.conn, &image_ids, &group_id, |_| {
                core_library::Event {
                    event_type: "culling.label".into(),
                    color_label: label.clone(),
                    ..Default::default()
                }
            });
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Count + on-disk size of the rejected photos matching `params` — what `cull_delete_rejected`
/// would remove. Read-only; drives the confirmation dialog so the user sees the real number rather
/// than the grid's (possibly paged) one.
#[tauri::command]
pub async fn cull_rejected_summary(
    app: AppHandle,
    params: QueryParams,
) -> Result<core_library::RejectSummary, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_rejected_summary",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::summarize_rejected(&db.conn, &params).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Send every rejected photo matching `params` (plus the camera companions of a rejected RAW) to the
/// OS Trash and drop its catalog rows.
///
/// The frontend passes only the *filter*, never image ids: the target set is re-derived here with
/// the `reject` flag forced on (`core_library::rejected_ids`), so a stale or malformed request can
/// never reach a picked or unflagged photo. Files remain recoverable from the Trash; the catalog
/// removal is not undoable in-app. The Trash moves run with neither the catalog nor the publication
/// gate locked (see [`crate::trash_flow`]); a catalog reset mid-run leaves the new catalog untouched.
#[tauri::command]
pub async fn cull_delete_rejected(
    app: AppHandle,
    params: QueryParams,
) -> Result<core_library::RejectDeleteResult, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "cull_delete_rejected",
        move || {
            let st = app.state::<AppState>();
            crate::trash_flow::run_delete_rejected(st.inner(), &params)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Keywords / tags ----------

#[tauri::command]
pub async fn keywords_list(app: AppHandle) -> Result<Vec<KeywordRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "keywords_list",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::list_keywords(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keywords_for_image(app: AppHandle, image_id: i64) -> Result<Vec<KeywordRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "keywords_for_image",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::keywords_for_image(&db.conn, image_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_add_to_image(
    app: AppHandle,
    image_id: i64,
    name: String,
) -> Result<KeywordRow, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "keyword_add_to_image",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let row = core_library::add_keyword_to_image(&db.conn, image_id, &name)
                .map_err(|e| e.to_string())?;
            sync_sidecar(st.inner(), image_id);
            Ok(row)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_add_to_images(
    app: AppHandle,
    image_ids: Vec<i64>,
    name: String,
) -> Result<KeywordRow, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "keyword_add_to_images",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let row = core_library::add_keyword_to_images(&db.conn, &image_ids, &name)
                .map_err(|e| e.to_string())?;
            sync_sidecars(st.inner(), &image_ids);
            Ok(row)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_remove_from_image(
    app: AppHandle,
    image_id: i64,
    keyword_id: i64,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "keyword_remove_from_image",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::remove_keyword_from_image(&db.conn, image_id, keyword_id)
                .map_err(|e| e.to_string())?;
            sync_sidecar(st.inner(), image_id);
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn keyword_delete(app: AppHandle, keyword_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "keyword_delete",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            // Capture images that carry this keyword BEFORE the cascade delete, to rewrite their sidecars.
            let affected: Vec<i64> = {
                let mut stmt = db
                    .conn
                    .prepare("SELECT image_id FROM image_keywords WHERE keyword_id = ?1")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([keyword_id], |r| r.get::<_, i64>(0))
                    .map_err(|e| e.to_string())?;
                rows.filter_map(Result::ok).collect()
            };
            core_library::delete_keyword(&db.conn, keyword_id).map_err(|e| e.to_string())?;
            sync_sidecars(st.inner(), &affected);
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Collections ----------

#[tauri::command]
pub async fn collections_list(app: AppHandle) -> Result<Vec<CollectionRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "collections_list",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::list_collections(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collections_for_image(
    app: AppHandle,
    image_id: i64,
) -> Result<Vec<CollectionRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "collections_for_image",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::collections_for_image(&db.conn, image_id).map_err(|e| e.to_string())
        },
    )
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
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "collection_create",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::create_collection(&db.conn, &name, is_smart, query.as_deref())
                .map_err(|e| e.to_string())
        },
    )
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
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "collection_add_images",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::add_images_to_collection(&db.conn, collection_id, &image_ids)
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn collection_remove_images(
    app: AppHandle,
    collection_id: i64,
    image_ids: Vec<i64>,
) -> Result<usize, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "collection_remove_images",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::remove_images_from_collection(&db.conn, collection_id, &image_ids)
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// The copy/move import destination. Prefers the user-configured library root (Settings); falls back
/// to the parent of the first watched folder (the root under which dated `YYYY/YYYY-MM-DD` folders
/// live) for catalogs created before the setting existed; `None` if neither is available.
#[tauri::command]
pub async fn app_library_root(app: AppHandle) -> Result<Option<String>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "app_library_root",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            if let Some(root) = core_library::library_root(&db.conn).map_err(|e| e.to_string())? {
                return Ok::<_, String>(Some(root));
            }
            let first: Option<String> = db
                .conn
                .query_row("SELECT path FROM folders ORDER BY id LIMIT 1", [], |r| {
                    r.get(0)
                })
                .ok();
            Ok::<_, String>(first.and_then(|p| {
                Path::new(&p)
                    .parent()
                    .map(|parent| parent.display().to_string())
            }))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Persist the library root (the copy/move import destination), creating the directory if needed.
/// Existing photos are NOT moved — only future copy/move imports file into the new location.
#[tauri::command]
pub async fn set_library_root(app: AppHandle, path: String) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_library_root",
        move || {
            std::fs::create_dir_all(&path).map_err(|e| format!("cannot create {path}: {e}"))?;
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_library_root(&db.conn, &path).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Dedup ----------

#[tauri::command]
pub async fn dedup_scan(
    app: AppHandle,
    category: String,
) -> Result<Vec<core_dedup::DupGroup>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "dedup_scan",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            match category.as_str() {
                "capture" => core_dedup::find_same_capture(&db.conn),
                _ => core_dedup::find_byte_identical(&db.conn),
            }
            .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Resolve one duplicate group: keep `keep_id`, send `trash_ids` to the OS Trash and drop their
/// rows. The Trash moves run with neither the catalog nor the publication gate locked (see
/// [`crate::trash_flow`]); a catalog reset mid-run leaves the new catalog untouched and reports 0.
#[tauri::command]
pub async fn dedup_resolve(
    app: AppHandle,
    keep_id: i64,
    trash_ids: Vec<i64>,
    // Optional decision context (frontend passes from the DupGroup): the full set shown, the rule's
    // suggested keeper, and the group key. Backward-compatible — absent → derived/None.
    candidate_ids: Option<Vec<i64>>,
    auto_keeper_id: Option<i64>,
    group_id: Option<String>,
) -> Result<usize, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "dedup_resolve",
        move || {
            let st = app.state::<AppState>();
            crate::trash_flow::run_resolve(
                st.inner(),
                keep_id,
                &trash_ids,
                candidate_ids,
                auto_keeper_id,
                group_id,
            )
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Auto-resolve all byte-identical duplicate groups (keep one copy each, trash the rest). Returns
/// the number of files trashed. Same-capture/perceptual matches are never auto-resolved. The Trash
/// moves run with neither the catalog nor the publication gate locked (see [`crate::trash_flow`]);
/// a catalog reset mid-run leaves the new catalog untouched and reports 0.
#[tauri::command]
pub async fn dedup_resolve_bulk(app: AppHandle) -> Result<usize, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "dedup_resolve_bulk",
        move || {
            let st = app.state::<AppState>();
            crate::trash_flow::run_resolve_bulk(st.inner())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Perceptual same-scene scan. Lazily computes versioned visual features for any present image that
/// lacks them (parallel decode of cached thumbnails, emitting `dedup:progress`), then groups only
/// capture-time-gated, visually verified candidates. Never auto-trashes.
#[tauri::command]
pub async fn dedup_scan_perceptual(
    app: AppHandle,
    threshold: u32,
) -> Result<Vec<core_dedup::DupGroup>, String> {
    let app2 = app.clone();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "dedup_scan_perceptual",
        move || {
            let st = app2.state::<AppState>();

            // Present rows lacking current similarity features: (id, lowercase-hex content hash).
            let todo: Vec<(i64, String)> = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                let mut stmt = db
                    .conn
                    .prepare(
                        "SELECT i.id, lower(hex(i.content_hash)) FROM images i \
                     LEFT JOIN image_similarity_features sf \
                       ON sf.image_id=i.id AND sf.feature_version=?1 \
                     WHERE i.status='present' AND sf.image_id IS NULL",
                    )
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map(
                        core_db::rusqlite::params![core_dedup::SIMILARITY_FEATURE_VERSION],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .map_err(|e| e.to_string())?
                    .collect::<core_db::rusqlite::Result<Vec<(i64, String)>>>()
                    .map_err(|e| e.to_string())?;
                rows
            };

            // Compute visual features sequentially from cached thumbnails, then persist in one tx.
            if !todo.is_empty() {
                let total = todo.len();
                let done = AtomicUsize::new(0);
                let computed: Vec<(i64, core_dedup::SimilarityFeatures)> =
                    todo.iter()
                        .filter_map(|(id, hex)| {
                            let features =
                                st.thumbs.read(hex, core_library::THUMB_SIZE).ok().and_then(
                                    |bytes| core_dedup::similarity_features_from_jpeg(&bytes),
                                );
                            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                            if n == total || n.is_multiple_of(16) {
                                let _ = app2.emit(
                                    "dedup:progress",
                                    serde_json::json!({"done": n, "total": total}),
                                );
                            }
                            features.map(|f| (*id, f))
                        })
                        .collect();

                let mut db = st.db.lock().map_err(|e| e.to_string())?;
                let tx = db.conn.transaction().map_err(|e| e.to_string())?;
                {
                    let mut upsert = tx
                        .prepare(
                            "INSERT INTO image_similarity_features(
                            image_id, feature_version, dhash64, phash64,
                            tiny_luma32, color_grid4x4, computed_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%s','now'))
                         ON CONFLICT(image_id) DO UPDATE SET
                            feature_version=excluded.feature_version,
                            dhash64=excluded.dhash64,
                            phash64=excluded.phash64,
                            tiny_luma32=excluded.tiny_luma32,
                            color_grid4x4=excluded.color_grid4x4,
                            computed_at=excluded.computed_at",
                        )
                        .map_err(|e| e.to_string())?;
                    let mut legacy = tx
                        .prepare("UPDATE images SET phash=?1 WHERE id=?2")
                        .map_err(|e| e.to_string())?;
                    for (id, f) in &computed {
                        upsert
                            .execute(core_db::rusqlite::params![
                                id,
                                core_dedup::SIMILARITY_FEATURE_VERSION,
                                f.dhash64 as i64,
                                f.phash64 as i64,
                                &f.tiny_luma32,
                                &f.color_grid4x4,
                            ])
                            .map_err(|e| e.to_string())?;
                        legacy
                            .execute(core_db::rusqlite::params![f.dhash64 as i64, id])
                            .map_err(|e| e.to_string())?;
                    }
                }
                tx.commit().map_err(|e| e.to_string())?;
            }

            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_dedup::find_perceptual(&db.conn, threshold).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Import ----------

/// Metadata to apply to every freshly-imported row (Lightroom "Apply During Import"). All optional.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOptions {
    /// Star rating 0–5 to stamp on imported images.
    pub rating: Option<i64>,
    /// Flag ("pick" | "reject"); "none"/absent leaves it unset.
    pub flag: Option<String>,
    /// Keyword names to attach (created on demand).
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Existing collection to add the batch to.
    pub collection_id: Option<i64>,
    /// New collection name (created when `collection_id` is absent).
    pub new_collection: Option<String>,
}

/// Apply on-import options to the freshly-added rows under one brief lock (reuses the same
/// core-library batch fns that back the cull / keyword / collection IPC commands), fenced on the
/// catalog generation the import started on. A catalog replaced underneath the import holds none of
/// these rows, so a stale epoch is not an error — there is simply nothing to apply.
fn apply_import_options(
    st: &AppState,
    ids: &[i64],
    opts: &ImportOptions,
    epoch: core_model::identity::CatalogEpoch,
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    // Lock order: publication gate before the catalog lock.
    match st.publications.with_epoch(epoch, || -> Result<(), String> {
        let mut db = st.db.lock().map_err(|e| e.to_string())?;
        if let Some(stars) = opts.rating {
            core_library::set_rating_many(&mut db.conn, ids, stars).map_err(|e| e.to_string())?;
        }
        if let Some(flag) = opts.flag.as_deref() {
            if flag != "none" && !flag.is_empty() {
                core_library::set_flag_many(&mut db.conn, ids, flag).map_err(|e| e.to_string())?;
            }
        }
        for kw in &opts.keywords {
            let kw = kw.trim();
            if !kw.is_empty() {
                core_library::add_keyword_to_images(&db.conn, ids, kw)
                    .map_err(|e| e.to_string())?;
            }
        }
        let collection_id = match (opts.collection_id, opts.new_collection.as_deref()) {
            (Some(id), _) => Some(id),
            (None, Some(name)) if !name.trim().is_empty() => Some(
                core_library::create_collection(&db.conn, name.trim(), false, None)
                    .map_err(|e| e.to_string())?,
            ),
            _ => None,
        };
        if let Some(cid) = collection_id {
            core_library::add_images_to_collection(&db.conn, cid, ids)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }) {
        Ok(applied) => applied,
        Err(core_runtime::publication::Stale) => {
            tracing::warn!("catalog replaced during import; options skipped");
            Ok(())
        }
    }
}

/// Fast import listing: enumerate the RAW files under `source` from filesystem metadata only — NO
/// catalog access, hashing, decode, or thumbnail work — so a full card lists in milliseconds. Every
/// file comes back `pending`; hash-verified dedup runs separately via `import_dedup`. Thumbnails are
/// fetched lazily, per file, via `import_thumb`.
#[tauri::command]
pub async fn import_list(
    app: AppHandle,
    source: String,
    recursive: Option<bool>,
) -> Result<Vec<core_import::SourceFile>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Import,
        false,
        "import_list",
        move || {
            Ok::<_, String>(core_import::list_source(
                Path::new(&source),
                recursive.unwrap_or(true),
            ))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Hash-verified dedup for the listed source files: classify each path as new / library-duplicate /
/// batch-duplicate by BLAKE3 (size-prefiltered, so only genuine candidates are read). Runs in the
/// background while the list is already visible; emits `import:dedup:progress {done,total,results}`
/// as it resolves files, and returns the full verdict set.
#[tauri::command]
pub async fn import_dedup(
    app: AppHandle,
    paths: Vec<String>,
) -> Result<Vec<core_import::DedupResult>, String> {
    let app2 = app.clone();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Import,
        true,
        "import_dedup",
        move || {
            let st = app2.state::<AppState>();
            // Brief lock: snapshot present content hashes + sizes (the size set drives the prefilter).
            let (present_hashes, present_sizes) = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                let mut stmt = db
                    .conn
                    .prepare("SELECT content_hash, file_size FROM images WHERE status = 'present'")
                    .map_err(|e| e.to_string())?;
                let rows = stmt
                    .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))
                    .map_err(|e| e.to_string())?;
                let mut hashes = std::collections::HashSet::new();
                let mut sizes = std::collections::HashSet::new();
                for row in rows.filter_map(Result::ok) {
                    if row.0.len() == 32 {
                        let mut a = [0u8; 32];
                        a.copy_from_slice(&row.0);
                        hashes.insert(a);
                    }
                    sizes.insert(row.1);
                }
                (hashes, sizes)
            };
            let files: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
            let progress_app = app2.clone();
            let results = core_import::dedup_scan(
                &files,
                &present_hashes,
                &present_sizes,
                |done, total, batch| {
                    let _ = progress_app.emit(
                        "import:dedup:progress",
                        serde_json::json!({"done": done, "total": total, "results": batch}),
                    );
                },
            );
            Ok::<_, String>(results)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Lazy single-file preview for the import list: decode just this one file's embedded preview and
/// return JPEG bytes. Called on demand (when the user clicks a row), never up front — keeps listing
/// instant. No catalog access.
#[tauri::command]
pub async fn import_thumb(
    app: AppHandle,
    path: String,
    max_edge: Option<u32>,
) -> Result<tauri::ipc::Response, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Import,
        true,
        "import_thumb",
        move || {
            let edge = max_edge.unwrap_or(1024);
            let src = core_raw::source_from_path(Path::new(&path)).map_err(|e| e.to_string())?;
            let thumb = core_raw::thumbnail_jpeg(&src, edge, 85).map_err(|e| e.to_string())?;
            Ok::<_, String>(tauri::ipc::Response::new(thumb.jpeg))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Commit a staged import: copy/move/reference only the `selected` source files, then apply
/// `options` to the freshly-added rows. Emits `import:progress` (live rows) + `import:done`.
///
/// Cancellable and non-durable: a job cancel or app shutdown stops the run between chunks, and the
/// `import:done` stats then describe exactly what was committed (`interrupted`). Rows already
/// committed stay committed — the job is marked committed at its first catalog write, so shutdown
/// never waits for the rest of the batch.
///
/// `pairing` decides what happens to a JPEG/HEIF the camera wrote beside a RAW: `"pair"` links it
/// to that RAW (one grid cell), anything else (the default) catalogs every file standalone.
#[tauri::command]
pub async fn import_commit(
    app: AppHandle,
    source: String,
    mode: String,
    dest: String,
    selected: Vec<String>,
    options: ImportOptions,
    pairing: Option<String>,
) -> Result<core_import::ImportStats, String> {
    let app2 = app.clone();
    crate::jobs::spawn_job(
        app.clone(),
        core_app::jobs::JobClass::Import,
        false,
        "import_commit",
        move |ctx| {
            let st = app2.state::<AppState>();
            let import_mode = match mode.as_str() {
                "move" => core_import::ImportMode::Move,
                "reference" => core_import::ImportMode::Reference,
                _ => core_import::ImportMode::Copy,
            };
            let pairing = match pairing.as_deref() {
                Some("pair") => core_import::Pairing::Pair,
                _ => core_import::Pairing::Standalone,
            };
            let files: Vec<PathBuf> = selected.iter().map(PathBuf::from).collect();
            // Gate the FS watcher for the duration of the import so it can't race the now-unlocked
            // per-file copy/process phase (double-decode / duplicate insert). Dropped when this closure
            // returns (success or error), which runs one deferred watcher sync if one was suppressed.
            let _import_guard = crate::watch::ImportGuard::new(app2.clone());
            let progress_app = app2.clone();
            // Buffer freshly-imported rows (flush with progress for live grid append) and collect their
            // ids for the on-import options pass (single-threaded blocking import → RefCell is enough).
            let pending = std::cell::RefCell::new(Vec::<core_library::ImageRow>::new());
            let added_ids = std::cell::RefCell::new(Vec::<i64>::new());
            // Cooperative cancellation (job cancel / app shutdown) plus the catalog generation the
            // run started on, which every catalog commit below is fenced on.
            let control = crate::import_control::HostImportControl::new(&st, ctx);
            let stats = core_import::import_files(
                &st.db,
                &st.thumbs,
                Path::new(&source),
                &files,
                import_mode,
                Path::new(&dest),
                pairing,
                |done, total, added| {
                    if let Some(row) = added {
                        added_ids.borrow_mut().push(row.id);
                        pending.borrow_mut().push(row.clone());
                    }
                    // Flush on completion, a full batch, or every 4th file (keeps the counter live even
                    // through runs of skipped files that add no rows).
                    if done == total || pending.borrow().len() >= 8 || done.is_multiple_of(4) {
                        let images: Vec<core_library::ImageRow> =
                            pending.borrow_mut().drain(..).collect();
                        let _ = progress_app.emit(
                            "import:progress",
                            serde_json::json!({"done": done, "total": total, "images": images}),
                        );
                    }
                },
                &control,
            )
            .map_err(|e| e.to_string())?;

            apply_import_options(&st, &added_ids.into_inner(), &options, control.epoch())?;
            enforce_thumb_cap(&st);
            // Always report the run: an interrupted one still has committed rows the grid must show.
            let _ = app2.emit("import:done", &stats);
            if !st.workers.is_stopping() {
                crate::thumb_queue::enqueue_all(&app2);
                // Freshly-imported photos get their feature vectors without the user having to know the
                // Settings button exists. Background + skips images that already have a row.
                crate::features::spawn_backfill(&app2);
            }
            Ok::<_, String>(stats)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Settings ----------

#[tauri::command]
pub fn logs_status() -> Result<crate::logging::LogsStatus, String> {
    crate::logging::status()
}

#[tauri::command]
pub async fn set_logs_directory(
    app: AppHandle,
    path: String,
) -> Result<crate::logging::LogsStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_logs_directory",
        move || crate::logging::set_directory(Path::new(&path)),
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn set_log_level(level: String) -> Result<crate::logging::LogsStatus, String> {
    crate::logging::set_level(&level)
}

#[tauri::command]
pub async fn logs_export_zip(app: AppHandle, dest: String) -> Result<u64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Export,
        true,
        "logs_export_zip",
        move || crate::logging::export_zip(Path::new(&dest)),
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn logs_delete_all() -> Result<crate::logging::LogsStatus, String> {
    crate::logging::delete_all()
}

#[tauri::command]
pub async fn catalog_backup_now(app: AppHandle) -> Result<crate::backup::BackupStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Maintenance,
        true,
        "catalog_backup_now",
        move || crate::backup::run_now(&app),
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn catalog_backup_status(app: AppHandle) -> Result<crate::backup::BackupStatus, String> {
    crate::backup::status(&app)
}

/// Configured thumbnail-cache cap in bytes (default when unset).
#[tauri::command]
pub async fn thumb_cache_cap(app: AppHandle) -> Result<u64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "thumb_cache_cap",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::thumb_cache_cap(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Current on-disk size of the thumbnail cache in bytes.
#[tauri::command]
pub async fn thumb_cache_size(app: AppHandle) -> Result<u64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "thumb_cache_size",
        move || {
            let st = app.state::<AppState>();
            st.thumbs.total_size().map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Persist a new thumbnail-cache cap and immediately evict down to it. Returns bytes freed.
#[tauri::command]
pub async fn set_thumb_cache_cap(app: AppHandle, bytes: u64) -> Result<u64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_thumb_cache_cap",
        move || {
            let st = app.state::<AppState>();
            {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::set_thumb_cache_cap(&db.conn, bytes).map_err(|e| e.to_string())?;
            }
            st.thumbs.evict_to(bytes).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Configured display-sharp preview longest edge in px, or `0` when unset (frontend picks a default
/// from the display on first launch).
#[tauri::command]
pub async fn preview_edge(app: AppHandle) -> Result<u32, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "preview_edge",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::preview_edge(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Persist the preview longest edge (clamped). Re-enqueues the whole library so images that were
/// deferred (no preview because the edge was unset) — and any whose preview edge changed — render at
/// the new size; already-current tiers are cheaply skipped by the worker's pre-check.
#[tauri::command]
pub async fn set_preview_edge(app: AppHandle, edge: u32) -> Result<(), String> {
    {
        let st = app.state::<AppState>();
        let db = st.db.lock().map_err(|e| e.to_string())?;
        core_library::set_preview_edge(&db.conn, edge).map_err(|e| e.to_string())?;
    }
    crate::thumb_queue::enqueue_all(&app);
    Ok(())
}

// ---------- AI analysis ----------

/// Total / analyzed / pending image counts + models-ready + running flags (for the Detected panel).
#[tauri::command]
pub async fn analysis_status(app: AppHandle) -> Result<crate::analysis::AnalysisStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "analysis_status",
        move || {
            let st = app.state::<AppState>();
            crate::analysis::status(&st)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Download any missing model files (first run only). Emits `analysis:models` progress.
#[tauri::command]
pub async fn analysis_models_ensure(app: AppHandle) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "analysis_models_ensure",
        move || crate::analysis::ensure_models(&app),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Run the background analysis pass. `force` re-analyzes everything. Emits `analysis:progress`/`:done`.
///
/// `scope` narrows the pass to the container the grid is showing. The frontend may post its whole
/// filter — `ScanScope` deserializes only the container dimensions, so rating/flag/search and the
/// scan-derived `detectedCategory`/`personId` are dropped by the type. Omitted ⇒ whole library.
///
/// Legacy entry point: it runs every AI stage. The Run-AI-scan modal calls `scan_run` instead, which
/// owns stage selection and sequences panorama detection in the same job.
#[tauri::command]
pub async fn analysis_run(
    app: AppHandle,
    force: bool,
    scope: Option<core_library::ScanScope>,
) -> Result<crate::analysis::RunStats, String> {
    let scope = scope.unwrap_or_default();
    let stages = crate::scan::AI_STAGES.to_vec();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "analysis_run",
        move || crate::analysis::run_pass(&app, force, &scope, &stages),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Run the unified AI scan: the selected stages, in one job, with one cancel.
///
/// This is what the "Run AI scan…" modal calls. Unlike `analysis_run` it also sequences panorama
/// detection, and its cancel works during the model download — the window in which `analysis_cancel`
/// is still a no-op.
#[tauri::command]
pub async fn scan_run(
    app: AppHandle,
    selection: crate::scan::ScanSelection,
) -> Result<crate::scan::ScanResult, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "scan_run",
        move || crate::scan::run(&app, &selection),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Request the running unified scan to stop, whatever phase it is in. Everything already committed
/// is kept. No-op if idle.
#[tauri::command]
pub fn scan_cancel(app: AppHandle) {
    crate::jobs::cancel_class(
        app.state::<AppState>().inner(),
        core_app::jobs::JobClass::Analysis,
    );
    crate::scan::cancel(&app.state::<AppState>());
}

/// Whether a unified scan is in flight. Lets the UI re-attach after a reload rather than showing an
/// idle button while a scan is still running.
#[tauri::command]
pub fn scan_running(app: AppHandle) -> bool {
    crate::scan::is_running(&app.state::<AppState>())
}

/// The scan modal's remembered stage ticks. Typed + validated in Rust rather than letting the
/// frontend write `app_meta` directly — the IPC boundary is the contract, and an unrecognised stage
/// id must never reach a run.
#[tauri::command]
pub async fn scan_prefs_get(app: AppHandle) -> Result<Vec<core_library::StageId>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "scan_prefs_get",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::scan_stages(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn scan_prefs_set(
    app: AppHandle,
    stages: Vec<core_library::StageId>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "scan_prefs_set",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_scan_stages(&db.conn, &stages).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// The per-photo scan record for the info panel: which stages ran, when, and which failed.
#[tauri::command]
pub async fn image_scan_state(
    app: AppHandle,
    image_id: i64,
) -> Result<core_library::ImageScanState, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_scan_state",
        move || crate::analysis::image_scan_state(&app.state::<AppState>(), image_id),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// How much work the AI scans have inside `scope` — sizes the "Analyze · N" / "Find People · N"
/// buttons. Read-only and model-free, so it is safe to call on every filter change.
#[tauri::command]
pub async fn scan_scope_counts(
    app: AppHandle,
    scope: Option<core_library::ScanScope>,
) -> Result<core_library::ScopeCounts, String> {
    let scope = scope.unwrap_or_default();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "scan_scope_counts",
        move || crate::analysis::scope_counts(&app.state::<AppState>(), &scope),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Request the running analysis pass to stop after the current batch commits. Results already
/// persisted are kept; the pass then emits `analysis:done` with its partial stats. No-op if idle.
#[tauri::command]
pub fn analysis_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.analysis_running.load(Ordering::SeqCst) {
        st.analysis_cancel.store(true, Ordering::SeqCst);
    }
}

// ---------- AI denoise ----------

/// Denoise availability + running state (drives the Develop "Denoise" panel).
#[tauri::command]
pub async fn denoise_status(app: AppHandle) -> Result<crate::denoise::DenoiseStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "denoise_status",
        move || {
            let st = app.state::<AppState>();
            Ok(crate::denoise::status(&st))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Apply raw-domain denoise to `image_id` at `amount` (0..100), swapping the denoised source into the
/// render cache. Re-blends cached buffers on an amount change (no re-inference). Emits
/// `denoise:progress` / `denoise:done`; the frontend re-renders on `denoise:done`.
#[tauri::command]
pub async fn denoise_apply(app: AppHandle, image_id: i64, amount: f32) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "denoise_apply",
        move || crate::denoise::apply(&app, image_id, amount),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Turn denoise off for `image_id`: drops cached buffers + evicts the denoised render caches so the
/// next render re-decodes the clean image.
#[tauri::command]
pub async fn denoise_clear(app: AppHandle, image_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "denoise_clear",
        move || {
            let st = app.state::<AppState>();
            crate::denoise::clear(&st, image_id)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Request the running denoise compute to stop. No-op if idle.
#[tauri::command]
pub fn denoise_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.denoise_running.load(Ordering::SeqCst) {
        st.denoise_cancel.store(true, Ordering::SeqCst);
    }
}

// ---------- Panorama merge ----------

/// Fast low-res panorama stitch of `image_ids` for the merge dialog → JPEG bytes. First call for
/// an id list decodes + caches downscaled frames; projection/crop changes restitch from cache.
#[tauri::command]
pub async fn panorama_preview(
    app: AppHandle,
    image_ids: Vec<i64>,
    projection: String,
    boundary_warp: f32,
    auto_crop: bool,
) -> Result<tauri::ipc::Response, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Viewport,
        false,
        "panorama_preview",
        move || {
            crate::panorama::preview(&app, image_ids, projection, boundary_warp, auto_crop)
                .map(tauri::ipc::Response::new)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Full-resolution merge → LinearRaw DNG next to the first source, registered in the catalog and
/// linked to its sources. Resolves with the new image id when the whole job finishes; progress
/// streams via `panorama:progress` / `panorama:done`, failures also emit `panorama:error` so the
/// progress pill clears even if the caller dropped the promise.
#[tauri::command]
pub async fn panorama_merge(
    app: AppHandle,
    image_ids: Vec<i64>,
    projection: String,
    boundary_warp: f32,
    auto_crop: bool,
    detect_group_id: Option<i64>,
) -> Result<i64, String> {
    let emitter = app.clone();
    let result = crate::jobs::spawn_job(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "panorama_merge",
        move |ctx| {
            crate::panorama::merge(
                &app,
                ctx,
                image_ids,
                projection,
                boundary_warp,
                auto_crop,
                detect_group_id,
            )
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Err(msg) = &result {
        let _ = emitter.emit("panorama:error", serde_json::json!({ "message": msg }));
        // A failed merge can leave the dialog's cached preview frames stale (same cleanup the
        // success path does in panorama::merge) — drop them here too.
        let st = emitter.state::<AppState>();
        crate::panorama::clear_preview_cache(&st);
    }
    result
}

/// Request cancellation of the running merge (honored between sources/phases).
#[tauri::command]
pub fn panorama_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    crate::panorama::cancel(&st);
}

#[tauri::command]
pub async fn panorama_status(app: AppHandle) -> Result<crate::panorama::PanoStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "panorama_status",
        move || {
            let st = app.state::<AppState>();
            Ok(crate::panorama::status(&st))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Drop the merge dialog's cached preview frames (modal closed — they are only useful while the
/// dialog is re-stitching the same source set on option toggles).
#[tauri::command]
pub fn panorama_preview_release(app: AppHandle) {
    let st = app.state::<AppState>();
    crate::panorama::clear_preview_cache(&st);
}

// ---------- Panorama detection ----------

/// Run (or resume) a whole-library panorama-detection scan. Resolves with the accumulated count of
/// freshly-`suggested` groups when the scan finishes; progress streams via `pano_detect:progress` /
/// `pano_detect:done`, failures also emit `pano_detect:error` so the UI clears even if the caller
/// dropped the promise.
#[tauri::command]
pub async fn pano_detect_run(app: AppHandle, force: bool) -> Result<usize, String> {
    let emitter = app.clone();
    let result = crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "pano_detect_run",
        move || crate::pano_detect::run(&app, force),
    )
    .await
    .map_err(|e| e.to_string())?;
    if let Err(msg) = &result {
        let _ = emitter.emit("pano_detect:error", serde_json::json!({ "message": msg }));
    }
    result
}

/// Request cancellation of the running scan (honored between clusters).
#[tauri::command]
pub fn pano_detect_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    crate::pano_detect::cancel(&st);
}

#[tauri::command]
pub async fn pano_detect_status(
    app: AppHandle,
) -> Result<crate::pano_detect::PanoDetectStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "pano_detect_status",
        move || {
            let st = app.state::<AppState>();
            crate::pano_detect::status(&st)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Suggestion groups for the review panel.
#[tauri::command]
pub async fn pano_detect_groups(
    app: AppHandle,
    include_dismissed: bool,
) -> Result<Vec<core_library::pano_detect::PanoGroupRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "pano_detect_groups",
        move || {
            let st = app.state::<AppState>();
            crate::pano_detect::groups(&st, include_dismissed)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Dismiss (or undo a dismissal of) a suggestion group.
#[tauri::command]
pub async fn pano_detect_dismiss(
    app: AppHandle,
    group_id: i64,
    dismissed: bool,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "pano_detect_dismiss",
        move || {
            let st = app.state::<AppState>();
            crate::pano_detect::dismiss(&st, group_id, dismissed)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Record that a suggestion group was merged into `merged_image_id` (the merge-flow handoff).
#[tauri::command]
pub async fn pano_detect_mark_merged(
    app: AppHandle,
    group_id: i64,
    merged_image_id: i64,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "pano_detect_mark_merged",
        move || {
            let st = app.state::<AppState>();
            crate::pano_detect::mark_merged(&st, group_id, merged_image_id)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Manager overview of every AI model capability (Detection & Scene / People / AI Masking): install
/// state, on-disk size, and per-file + per-tier detail. Drives the AI Models manager UI.
#[tauri::command]
pub async fn models_overview(app: AppHandle) -> Result<Vec<crate::model_mgmt::ModelGroup>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "models_overview",
        move || {
            let st = app.state::<AppState>();
            Ok(vec![
                crate::analysis::overview(&st),
                crate::faces::overview(&st),
                crate::segment::overview(&st),
            ])
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Request an in-flight model download for `group` (`"analysis"`|`"faces"`|`"mask_ai"`) to stop. The
/// partially-written temp file is discarded; already-completed files stay. No-op if idle.
#[tauri::command]
pub fn models_cancel(app: AppHandle, group: String) {
    let st = app.state::<AppState>();
    match group.as_str() {
        "analysis" => st.analysis_dl_cancel.store(true, Ordering::SeqCst),
        "faces" => st.faces_dl_cancel.store(true, Ordering::SeqCst),
        "mask_ai" => st.mask_ai_dl_cancel.store(true, Ordering::SeqCst),
        _ => {}
    }
}

/// Delete a capability's downloaded model files to reclaim disk. For `"mask_ai"`, `tier` selects the
/// SAM tier (`"realtime"`|`"balanced"`|`"max"`; empty = active). Refuses while a scan is running so it
/// can't race a pass that's loading those weights.
#[tauri::command]
pub async fn models_remove(
    app: AppHandle,
    group: String,
    tier: Option<String>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "models_remove",
        move || {
            let st = app.state::<AppState>();
            if st.analysis_running.load(Ordering::SeqCst) {
                return Err("an AI scan is running — stop it before removing models".into());
            }
            match group.as_str() {
                "analysis" => crate::analysis::remove(&st),
                "faces" => crate::faces::remove(&st),
                "mask_ai" => crate::segment::remove(&st, tier.as_deref().unwrap_or("")),
                other => Err(format!("unknown model group: {other}")),
            }
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Download the interactive-segmentation (AI masking) model weights. Emits `mask_ai:models` progress.
#[tauri::command]
pub async fn mask_ai_models_ensure(app: AppHandle) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "mask_ai_models_ensure",
        move || crate::segment::ensure_models(&app),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Whether the AI-masking models are downloaded and ready.
#[tauri::command]
pub fn mask_ai_ready(app: AppHandle) -> bool {
    let st = app.state::<AppState>();
    crate::segment::models_ready(&st)
}

/// Warm the SAM image embedding for `image_id` in the background (downloads the tier's model on first
/// use, then runs the heavy encode) so the first click segments instantly. Fire-and-forget from the UI.
#[tauri::command]
pub async fn mask_ai_encode(app: AppHandle, image_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "mask_ai_encode",
        move || {
            crate::segment::ensure_models(&app)?;
            let st = app.state::<AppState>();
            crate::segment::warm(&st, image_id)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Compute an AI (SAM) mask from prompt points on `image_id`. Returns the component key (`hash`) to
/// store in the mask's `ai` component plus the mask dims + predicted IoU. The heavy image encode runs
/// once per image and is cached, so repeated clicks are fast.
#[tauri::command]
pub async fn mask_ai_prompt(
    app: AppHandle,
    image_id: i64,
    points: Vec<core_pipeline::AiPoint>,
) -> Result<crate::segment::PromptResult, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "mask_ai_prompt",
        move || {
            // Fetch the (selected-tier) SAM weights on first use — idempotent, skips present files, emits
            // `mask_ai:models` progress. Without this the first prompt errors "models not downloaded".
            crate::segment::ensure_models(&app)?;
            let st = app.state::<AppState>();
            crate::segment::prompt(&st, image_id, points)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Detected-object category counts (distinct images) for the LeftNav facet.
#[tauri::command]
pub async fn analysis_facets(app: AppHandle) -> Result<Vec<FacetRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "analysis_facets",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::analysis_facets(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Detected objects for one image (metadata panel).
#[tauri::command]
pub async fn image_detections(app: AppHandle, id: i64) -> Result<Vec<DetectionRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_detections",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::detections_for_image(&db.conn, id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Generated caption + keywords for one image (metadata panel).
#[tauri::command]
pub async fn image_caption(app: AppHandle, id: i64) -> Result<Option<CaptionRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_caption",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::caption_for_image(&db.conn, id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// MobileCLIP presence-probe scores for one image (advisory AI readout; `None` until the probe ran).
#[tauri::command]
pub async fn image_presence(app: AppHandle, id: i64) -> Result<Option<PresenceRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_presence",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::presence_for_image(&db.conn, id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---- Faces / People ----

/// People status: present-image total, how many are face-processed, total faces/people, model state.
#[tauri::command]
pub async fn faces_status(app: AppHandle) -> Result<crate::faces::FacesStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "faces_status",
        move || {
            let st = app.state::<AppState>();
            crate::faces::status(&st)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Download any missing face models (~190 MB, first run only). Emits `faces:models` progress.
#[tauri::command]
pub async fn faces_models_ensure(app: AppHandle) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "faces_models_ensure",
        move || crate::faces::ensure_face_models(&app),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Run the "Find People" pass (detect → align → embed → cluster). `force` re-processes everything.
/// Emits `faces:progress`/`faces:done`.
///
/// `scope` narrows *detection + embedding* to the container the grid is showing (see
/// [`analysis_run`]). Clustering stays library-wide by design: a face found in one folder must still
/// be matchable against people discovered anywhere else.
#[tauri::command]
pub async fn faces_run(
    app: AppHandle,
    force: bool,
    scope: Option<core_library::ScanScope>,
) -> Result<crate::faces::FacesRunStats, String> {
    let scope = scope.unwrap_or_default();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "faces_run",
        move || crate::faces::run_pass(&app, force, &scope),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Request the running scan to stop after the current batch commits. No-op if idle. Faces run inside
/// the unified scan, so this shares the analysis cancel flag.
#[tauri::command]
pub fn faces_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.analysis_running.load(Ordering::SeqCst) {
        st.analysis_cancel.store(true, Ordering::SeqCst);
    }
}

/// Whether the face stage participates in the unified AI scan (Settings toggle; default on).
#[tauri::command]
pub async fn face_stage_enabled(app: AppHandle) -> Result<bool, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "face_stage_enabled",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::face_stage_enabled(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Enable/disable the face stage in the unified scan.
#[tauri::command]
pub async fn set_face_stage_enabled(app: AppHandle, enabled: bool) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_face_stage_enabled",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_face_stage_enabled(&db.conn, enabled).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// People for the sidebar (named first, then unnamed "Suggested" clusters); each with a cover crop.
#[tauri::command]
pub async fn people_list(app: AppHandle, include_hidden: bool) -> Result<Vec<PersonRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "people_list",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::list_people(&db.conn, include_hidden).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Faces of one person, optionally restricted to a status (e.g. `"unconfirmed"` for the Review flow).
#[tauri::command]
pub async fn person_faces(
    app: AppHandle,
    person_id: i64,
    status: Option<String>,
) -> Result<Vec<PersonFaceRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "person_faces",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::person_faces(&db.conn, person_id, status.as_deref())
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Faces detected in one image (+ their person names) — the RightInfo "People" chips.
#[tauri::command]
pub async fn image_faces(app: AppHandle, id: i64) -> Result<Vec<ImageFaceRow>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_faces",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::image_faces(&db.conn, id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Set (or clear with `null`) a person's name.
#[tauri::command]
pub async fn person_set_name(
    app: AppHandle,
    person_id: i64,
    name: Option<String>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "person_set_name",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_person_name(
                &db.conn,
                person_id,
                name.as_deref(),
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Hide/unhide a person (excluded from the sidebar + library filter when hidden).
#[tauri::command]
pub async fn person_set_hidden(app: AppHandle, person_id: i64, hidden: bool) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "person_set_hidden",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_person_hidden(&db.conn, person_id, hidden, core_library::now_epoch())
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Set a person's cover (key) face. The face must belong to the person.
#[tauri::command]
pub async fn person_set_cover(app: AppHandle, person_id: i64, face_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "person_set_cover",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_person_cover(&db.conn, person_id, face_id, core_library::now_epoch())
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Merge person `src` into `dst` (move all faces + rejections, delete `src`). Atomic; not reversible.
#[tauri::command]
pub async fn person_merge(app: AppHandle, dst: i64, src: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "person_merge",
        move || {
            let st = app.state::<AppState>();
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            core_library::merge_people(&tx, dst, src).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Confirm a face belongs to its person ("yes" in the Review flow).
#[tauri::command]
pub async fn face_confirm(app: AppHandle, face_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "face_confirm",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::confirm_face(&db.conn, face_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Reject a face from its person ("not this person") — unlink + remember the rejection.
#[tauri::command]
pub async fn face_reject(app: AppHandle, face_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "face_reject",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::reject_face(&db.conn, face_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Reassign a face to a person (confirmed), or `null` to send it back to the suggestion pool.
#[tauri::command]
pub async fn face_assign(
    app: AppHandle,
    face_id: i64,
    person_id: Option<i64>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "face_assign",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::assign_face_person(&db.conn, face_id, person_id)
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Delete ALL face + person data (privacy "Delete all face data"). Atomic; not reversible.
#[tauri::command]
pub async fn faces_delete_all(app: AppHandle) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "faces_delete_all",
        move || {
            let st = app.state::<AppState>();
            // Guard against an in-flight scan repopulating the data we're about to wipe.
            if st.analysis_running.load(Ordering::SeqCst) {
                return Err("an AI scan is running — stop it before deleting face data".into());
            }
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            let tx = db.conn.transaction().map_err(|e| e.to_string())?;
            core_library::delete_all_face_data(&tx).map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Fit the pick/reject suggestion model on the current labels, in the background. Returns as soon as
/// the job is queued; the outcome arrives as `suggest:done` (a `TrainOutcome`) or `suggest:error`.
/// A promoted fit re-scores the library before `suggest:done` fires.
#[tauri::command]
pub async fn suggest_train(app: AppHandle) -> Result<(), String> {
    crate::suggest::spawn_train(&app)
}

/// The suggester's state: live-model metrics, the label census by provenance, and scored counts.
#[tauri::command]
pub async fn suggest_status(app: AppHandle) -> Result<core_library::SuggestStatus, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "suggest_status",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let mut status =
                core_library::suggest_status(&db.conn, crate::analysis::EMBEDDING_VERSION)
                    .map_err(|e| e.to_string())?;
            // The run flag lives in app state, not the catalog — see `SuggestStatus::running`.
            status.running = crate::suggest::is_running(&st);
            Ok(status)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Backfill per-image feature vectors (lighting/best-shot/dedup model inputs) for images missing
/// them. Explicit action; emits `features:progress`/`features:done`. Returns the count computed.
#[tauri::command]
pub async fn features_backfill(app: AppHandle) -> Result<usize, String> {
    let app2 = app.clone();
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "features_backfill",
        move || crate::features::run_backfill(&app2),
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Real per-image histogram for the Library metadata panel, computed from the cached thumbnail (no
/// GPU render needed). Reads the SAME canonical/edited render the grid shows (so it matches the
/// Develop histogram), falling back to the camera placeholder. `None` if nothing is cached yet.
#[tauri::command]
pub async fn image_histogram(app: AppHandle, image_id: i64) -> Result<Option<Histogram>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "image_histogram",
        move || {
            let st = app.state::<AppState>();
            let (hash, edit_version) = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                let hash = match core_library::image_by_id(&db.conn, image_id)
                    .map_err(|e| e.to_string())?
                {
                    Some(r) => r.content_hash,
                    None => return Ok(None),
                };
                let version = core_library::get_edit_with_version(&db.conn, image_id)
                    .ok()
                    .flatten()
                    .map(|(_, v)| v);
                (hash, version)
            };
            let jpeg = edit_version
                .and_then(|v| st.thumbs.read_edited(&hash, PROCESS_VERSION, v).ok())
                .or_else(|| st.thumbs.read_canonical(&hash, PROCESS_VERSION).ok())
                .or_else(|| st.thumbs.read(&hash, core_library::THUMB_SIZE).ok());
            let Some(jpeg) = jpeg else {
                return Ok(None);
            };
            Ok(core_pipeline::histogram_from_jpeg(&jpeg))
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Source frames behind `image_id` (an HDR bracket or a panorama stitch) for the metadata panel's
/// "Source frames" section. `None` when `image_id` is an ordinary, un-merged image.
#[tauri::command]
pub async fn image_sources(app: AppHandle, image_id: i64) -> Result<Option<MergeSources>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_sources",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::merge_sources(&db.conn, image_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// The RAW+JPEG/HEIF pair `image_id` belongs to, from either member, for the metadata panel.
/// `None` when the image is not paired.
#[tauri::command]
pub async fn image_pair(
    app: AppHandle,
    image_id: i64,
) -> Result<Option<core_library::PairInfo>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_pair",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::pair_info(&db.conn, image_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Break a pair: `secondary_id` (the camera companion) returns to the grid as a standalone image.
/// Files on disk are untouched — this only drops the catalog link.
#[tauri::command]
pub async fn image_pair_unlink(app: AppHandle, secondary_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_pair_unlink",
        move || {
            let st = app.state::<AppState>();
            {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::unlink_pair(&db.conn, secondary_id).map_err(|e| e.to_string())?;
            }
            // The companion re-enters the default grid query — refresh whoever is showing it.
            let _ = app.emit("library:changed", ());
            Ok::<_, String>(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Write a sidecar (`<raw>.json`: edits + rating + keywords) next to every present RAW — migrates an
/// existing catalog onto the durable on-disk format. Returns the count written.
#[tauri::command]
pub async fn sidecars_write_all(app: AppHandle) -> Result<usize, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Sidecar,
        true,
        "sidecars_write_all",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::write_all_sidecars(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Force-apply every present image's sidecar back into the catalog (recover edits/ratings/keywords
/// after a catalog loss or when moving between machines). Returns the count hydrated.
#[tauri::command]
pub async fn sidecars_rebuild(app: AppHandle) -> Result<usize, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Sidecar,
        true,
        "sidecars_rebuild",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::rebuild_from_sidecars(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Manual ground-truth labels for one image (the "Contains person/animal" checkboxes).
#[tauri::command]
pub async fn image_user_labels(app: AppHandle, id: i64) -> Result<UserLabels, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "image_user_labels",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::user_labels(&db.conn, id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Set one manual label field (`field` = "person" | "animal"; `value` = Some(bool) or None to clear).
#[tauri::command]
pub async fn set_image_user_label(
    app: AppHandle,
    id: i64,
    field: String,
    value: Option<bool>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_image_user_label",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_user_label(&db.conn, id, &field, value, core_library::now_epoch())
                .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Set one manual label field on many images at once (multi-select labeling). Logs one behavioral
/// event per image so the labels feed the AI training layer with their full candidate context.
#[tauri::command]
pub async fn set_image_user_label_many(
    app: AppHandle,
    image_ids: Vec<i64>,
    field: String,
    value: Option<bool>,
    group_id: Option<String>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_image_user_label_many",
        move || {
            let st = app.state::<AppState>();
            let mut db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_user_label_many(
                &mut db.conn,
                &image_ids,
                &field,
                value,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())?;
            let et = format!("label.{field}_set");
            let ctx = format!(
                "{{\"field\":\"{field}\",\"value\":{}}}",
                match value {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "null",
                }
            );
            log_batch(st.inner(), &db.conn, &image_ids, &group_id, |_| {
                core_library::Event {
                    event_type: et.clone(),
                    context: Some(ctx.clone()),
                    ..Default::default()
                }
            });
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Configured MegaDetector input size (640 or 1280).
#[tauri::command]
pub async fn analysis_detector_size(app: AppHandle) -> Result<u32, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "analysis_detector_size",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::animal_detector_size(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
fn clear_analyzer_cache(st: &AppState) -> Result<(), String> {
    *st.analyzers.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
fn clear_analyzer_cache(_st: &AppState) -> Result<(), String> {
    Ok(())
}

/// Set the MegaDetector input size; invalidates the cached analyzer registry so the next pass
/// rebuilds at the new resolution.
#[tauri::command]
pub async fn set_analysis_detector_size(app: AppHandle, size: u32) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "set_analysis_detector_size",
        move || {
            let st = app.state::<AppState>();
            {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::set_animal_detector_size(&db.conn, size)
                    .map_err(|e| e.to_string())?;
            }
            clear_analyzer_cache(&st)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Configured AI-masking quality tier (`"realtime"` | `"balanced"` | `"max"`).
#[tauri::command]
pub async fn mask_ai_tier_get(app: AppHandle) -> Result<String, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "mask_ai_tier_get",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::mask_ai_tier(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Set the AI-masking tier. Clears the cached SAM embedding so the next prompt re-encodes (a tier
/// change swaps the encoder); the segmenter itself rebuilds lazily when the tier differs.
#[tauri::command]
pub async fn mask_ai_tier_set(app: AppHandle, tier: String) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Analysis,
        false,
        "mask_ai_tier_set",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::set_mask_ai_tier(&db.conn, &tier).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Presets ----------

/// Portable preset file envelope (export/import) and the format of the bundled built-ins. `params` is
/// the sparse DevelopParams object (only the touched top-level fields). `schema_version` is REQUIRED
/// for a file to be treated as our native preset on import (its presence disambiguates our envelope
/// from an arbitrary `.json` that merely happens to have `params`/`fieldKeys`).
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetEnvelope {
    #[serde(default)]
    schema_version: Option<i64>,
    name: String,
    #[serde(default)]
    group_name: Option<String>,
    #[serde(default)]
    field_keys: Vec<String>,
    #[serde(default)]
    process_version: Option<i64>,
    params: serde_json::Value,
}

/// Result of importing a preset file: the new preset id + an honest conversion report.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetImportResult {
    preset_id: i64,
    report: core_preset::ImportReport,
}

const BUILTIN_PRESETS: &[&str] = &[
    include_str!("../resources/presets/punchy.json"),
    include_str!("../resources/presets/soft_matte.json"),
    include_str!("../resources/presets/bw_contrast.json"),
    include_str!("../resources/presets/warm_golden.json"),
    include_str!("../resources/presets/cool_shadows.json"),
];

/// Seed the bundled built-in presets (idempotent — `INSERT OR IGNORE`). Called once at setup.
pub fn seed_builtin_presets(conn: &core_db::rusqlite::Connection) {
    for raw in BUILTIN_PRESETS {
        match serde_json::from_str::<PresetEnvelope>(raw) {
            Ok(env) => {
                let group = env.group_name.as_deref().unwrap_or("Built-in");
                let keys = serde_json::to_string(&env.field_keys).unwrap_or_else(|_| "[]".into());
                let params = env.params.to_string();
                let pv = env.process_version.unwrap_or(PROCESS_VERSION);
                if let Err(e) = core_library::seed_builtin_preset(
                    conn,
                    &env.name,
                    group,
                    &keys,
                    &params,
                    pv,
                    core_library::now_epoch(),
                ) {
                    tracing::warn!(error = %crate::logging::safe_error(&e), "builtin preset seed failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "builtin preset parse failed"),
        }
    }
}

/// Parse the current stored edit (or defaults) as the JSON base to overlay a preset onto.
/// `replace_all` ⇒ start from `DevelopParams::default()` (preset becomes the complete look).
fn merge_base(
    conn: &core_db::rusqlite::Connection,
    image_id: i64,
    replace_all: bool,
) -> Result<serde_json::Value, String> {
    if replace_all {
        return serde_json::to_value(DevelopParams::default()).map_err(|e| e.to_string());
    }
    match core_library::get_edit(conn, image_id).map_err(|e| e.to_string())? {
        Some(json) => serde_json::from_str::<serde_json::Value>(&json).map_err(|e| e.to_string()),
        None => serde_json::to_value(DevelopParams::default()).map_err(|e| e.to_string()),
    }
}

fn sparse_map(params_json: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let v: serde_json::Value = serde_json::from_str(params_json).map_err(|e| e.to_string())?;
    v.as_object()
        .cloned()
        .ok_or_else(|| "preset params must be a JSON object".to_string())
}

/// Validate that a sparse params object overlays cleanly onto defaults and deserializes into a typed
/// `DevelopParams` — so a buggy importer / hand-edited preset fails at WRITE time (save/import) with a
/// clear error, instead of silently storing junk that only blows up later at apply.
fn validate_sparse_params(
    sparse: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let base = serde_json::to_value(DevelopParams::default()).map_err(|e| e.to_string())?;
    let merged = core_preset::apply_sparse(&base, sparse, 1.0);
    serde_json::from_value::<DevelopParams>(merged)
        .map(|_| ())
        .map_err(|e| format!("preset params are not valid develop settings: {e}"))
}

/// All presets (summaries) for the panel.
#[tauri::command]
pub async fn presets_list(app: AppHandle) -> Result<Vec<core_library::PresetSummary>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "presets_list",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::list_presets(&db.conn).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// One preset with its sparse params.
#[tauri::command]
pub async fn presets_get(
    app: AppHandle,
    preset_id: i64,
) -> Result<Option<core_library::PresetFull>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "presets_get",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::get_preset(&db.conn, preset_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Save the current edit (restricted to the checked `field_keys`) as a new user preset.
#[tauri::command]
pub async fn presets_save(
    app: AppHandle,
    name: String,
    group_name: Option<String>,
    field_keys: Vec<String>,
    is_favorite: Option<bool>,
    params: DevelopParams,
) -> Result<i64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "presets_save",
        move || {
            let full = serde_json::to_value(&params).map_err(|e| e.to_string())?;
            let sparse = core_preset::subset(&full, &field_keys);
            validate_sparse_params(&sparse)?;
            let sparse_json = serde_json::to_string(&serde_json::Value::Object(sparse))
                .map_err(|e| e.to_string())?;
            let keys_json = serde_json::to_string(&field_keys).map_err(|e| e.to_string())?;
            let group = group_name.unwrap_or_else(|| "My Presets".into());
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let name =
                core_library::unique_name(&db.conn, &group, &name).map_err(|e| e.to_string())?;
            core_library::insert_preset(
                &db.conn,
                &name,
                &group,
                is_favorite.unwrap_or(false),
                &keys_json,
                &sparse_json,
                PROCESS_VERSION,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Update a preset's mutable metadata. Built-ins may be favorited/reordered but not renamed/moved.
#[tauri::command]
pub async fn presets_update(
    app: AppHandle,
    preset_id: i64,
    name: Option<String>,
    group_name: Option<String>,
    is_favorite: Option<bool>,
    sort_order: Option<i64>,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "presets_update",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            if (name.is_some() || group_name.is_some())
                && core_library::is_builtin(&db.conn, preset_id).map_err(|e| e.to_string())?
            {
                return Err("built-in presets cannot be renamed or moved".to_string());
            }
            core_library::update_preset(
                &db.conn,
                preset_id,
                name.as_deref(),
                group_name.as_deref(),
                is_favorite,
                sort_order,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Delete a user preset (built-ins are refused).
#[tauri::command]
pub async fn presets_delete(app: AppHandle, preset_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "presets_delete",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            if core_library::is_builtin(&db.conn, preset_id).map_err(|e| e.to_string())? {
                return Err("built-in presets cannot be deleted".to_string());
            }
            core_library::delete_preset(&db.conn, preset_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Duplicate a preset (including a built-in) as a new editable user preset.
#[tauri::command]
pub async fn presets_duplicate(app: AppHandle, preset_id: i64) -> Result<i64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "presets_duplicate",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let p = core_library::get_preset(&db.conn, preset_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "preset not found".to_string())?;
            let group = if p.builtin {
                "My Presets".to_string()
            } else {
                p.group_name
            };
            let name = core_library::unique_name(&db.conn, &group, &format!("{} copy", p.name))
                .map_err(|e| e.to_string())?;
            let keys_json = serde_json::to_string(&p.field_keys).map_err(|e| e.to_string())?;
            core_library::insert_preset(
                &db.conn,
                &name,
                &group,
                false,
                &keys_json,
                &p.params,
                PROCESS_VERSION,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Apply a preset onto an image's current edit (or onto defaults when `replace_all`), blended by
/// `amount` (0..1, default 1). Returns the merged params — NOT persisted; the frontend commits.
#[tauri::command]
pub async fn presets_apply(
    app: AppHandle,
    image_id: i64,
    preset_id: i64,
    amount: Option<f32>,
    replace_all: Option<bool>,
) -> Result<DevelopParams, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "presets_apply",
        move || {
            let st = app.state::<AppState>();
            let _publication = st.publications.mutation_guard();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let preset = core_library::get_preset(&db.conn, preset_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "preset not found".to_string())?;
            let mut overlay = sparse_map(&preset.params)?;
            core_preset::migrate_sparse(&mut overlay, preset.process_version);
            let base = merge_base(&db.conn, image_id, replace_all.unwrap_or(false))?;
            let merged = core_preset::apply_sparse(&base, &overlay, amount.unwrap_or(1.0));
            serde_json::from_value::<DevelopParams>(merged).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Apply a copied source edit (subset to `field_keys`) onto an image — powers Copy/Paste settings.
/// Returns the merged params (not persisted).
#[tauri::command]
pub async fn develop_apply_settings(
    app: AppHandle,
    image_id: i64,
    params: DevelopParams,
    field_keys: Vec<String>,
    amount: Option<f32>,
    replace_all: Option<bool>,
) -> Result<DevelopParams, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "develop_apply_settings",
        move || {
            let full = serde_json::to_value(&params).map_err(|e| e.to_string())?;
            let overlay = core_preset::subset(&full, &field_keys);
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let base = merge_base(&db.conn, image_id, replace_all.unwrap_or(false))?;
            let merged = core_preset::apply_sparse(&base, &overlay, amount.unwrap_or(1.0));
            serde_json::from_value::<DevelopParams>(merged).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Export a preset to a portable JSON file at `dest_path`.
#[tauri::command]
pub async fn presets_export(
    app: AppHandle,
    preset_id: i64,
    dest_path: String,
) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Export,
        true,
        "presets_export",
        move || {
            let st = app.state::<AppState>();
            let p = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::get_preset(&db.conn, preset_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "preset not found".to_string())?
            };
            let env = PresetEnvelope {
                schema_version: Some(1),
                name: p.name,
                group_name: Some(p.group_name),
                field_keys: p.field_keys,
                process_version: Some(p.process_version),
                params: serde_json::from_str(&p.params).map_err(|e| e.to_string())?,
            };
            let json = serde_json::to_string_pretty(&env).map_err(|e| e.to_string())?;
            write_atomic(Path::new(&dest_path), json.as_bytes())?;
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Parse an import file into `(name, group, field_keys, sparse_params_json, report)`. Tries our native
/// envelope first, then the external-format registry (Lightroom etc., populated from Phase 3).
fn parse_import(
    bytes: &[u8],
    src_path: &str,
) -> Result<
    (
        String,
        String,
        Vec<String>,
        String,
        core_preset::ImportReport,
    ),
    String,
> {
    if let Ok(env) = serde_json::from_slice::<PresetEnvelope>(bytes) {
        // Require an explicit schemaVersion so an arbitrary `.json` that merely has `params`/`fieldKeys`
        // isn't mistaken for our native envelope (it falls through to the external-format registry).
        if env.schema_version.is_some() && env.params.is_object() && !env.field_keys.is_empty() {
            let mut report = core_preset::ImportReport {
                source_format: "native-json".into(),
                ..Default::default()
            };
            for k in &env.field_keys {
                report.mark_clean(k);
            }
            let group = env.group_name.unwrap_or_else(|| "Imported".into());
            let sparse_json = env.params.to_string();
            return Ok((env.name, group, env.field_keys, sparse_json, report));
        }
    }
    let registry = core_preset::Registry::with_defaults();
    let parsed = registry
        .import(bytes, Some(std::path::Path::new(src_path)))
        .map_err(|e| e.to_string())?;
    let name = std::path::Path::new(src_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Imported preset")
        .to_string();
    let sparse_json = serde_json::to_string(&serde_json::Value::Object(parsed.params))
        .map_err(|e| e.to_string())?;
    Ok((
        name,
        "Imported".into(),
        parsed.field_keys,
        sparse_json,
        parsed.report,
    ))
}

/// Import a preset file (native JSON now; Lightroom `.xmp`/`.lrtemplate` once Phase 3 registers them).
#[tauri::command]
pub async fn presets_import_file(
    app: AppHandle,
    src_path: String,
) -> Result<PresetImportResult, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Import,
        true,
        "presets_import_file",
        move || {
            // Bound the read before pulling the file into memory (a hostile/huge file can't be slurped).
            let size = std::fs::metadata(&src_path)
                .map_err(|e| e.to_string())?
                .len();
            if size > core_preset::MAX_IMPORT_BYTES as u64 {
                return Err(format!(
                    "preset file too large ({size} bytes; max {})",
                    core_preset::MAX_IMPORT_BYTES
                ));
            }
            let bytes = std::fs::read(&src_path).map_err(|e| e.to_string())?;
            let (name, group, field_keys, sparse_json, report) = parse_import(&bytes, &src_path)?;
            // Validate before storing so a buggy importer surfaces here, not at apply.
            validate_sparse_params(&sparse_map(&sparse_json)?)?;
            let keys_json = serde_json::to_string(&field_keys).map_err(|e| e.to_string())?;
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let name =
                core_library::unique_name(&db.conn, &group, &name).map_err(|e| e.to_string())?;
            let preset_id = core_library::insert_preset(
                &db.conn,
                &name,
                &group,
                false,
                &keys_json,
                &sparse_json,
                PROCESS_VERSION,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(PresetImportResult { preset_id, report })
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Develop snapshots (persistent history) ----------

/// Named snapshots for an image (newest first).
#[tauri::command]
pub async fn snapshots_list(
    app: AppHandle,
    image_id: i64,
) -> Result<Vec<core_library::SnapshotSummary>, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        false,
        "snapshots_list",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::list_snapshots(&db.conn, image_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Create a named snapshot of `params` for an image; returns the new id.
#[tauri::command]
pub async fn snapshot_create(
    app: AppHandle,
    image_id: i64,
    name: String,
    params: DevelopParams,
) -> Result<i64, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "snapshot_create",
        move || {
            let json = serde_json::to_string(&params).map_err(|e| e.to_string())?;
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let name = core_library::unique_snapshot_name(&db.conn, image_id, &name)
                .map_err(|e| e.to_string())?;
            core_library::create_snapshot(
                &db.conn,
                image_id,
                &name,
                &json,
                PROCESS_VERSION,
                core_library::now_epoch(),
            )
            .map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Restore a snapshot's params (validated through the current schema). NOT persisted — the frontend
/// commits the returned params so the restore becomes an undoable step.
#[tauri::command]
pub async fn snapshot_restore(app: AppHandle, snapshot_id: i64) -> Result<DevelopParams, String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "snapshot_restore",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            let (json, pv) = core_library::get_snapshot_params(&db.conn, snapshot_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "snapshot not found".to_string())?;
            let mut value: serde_json::Value =
                serde_json::from_str(&json).map_err(|e| e.to_string())?;
            core_preset::migrate_full(&mut value, pv);
            serde_json::from_value::<DevelopParams>(value).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Rename a snapshot.
#[tauri::command]
pub async fn snapshot_rename(app: AppHandle, snapshot_id: i64, name: String) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "snapshot_rename",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::rename_snapshot(&db.conn, snapshot_id, &name).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Delete a snapshot.
#[tauri::command]
pub async fn snapshot_delete(app: AppHandle, snapshot_id: i64) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::UserCommand,
        true,
        "snapshot_delete",
        move || {
            let st = app.state::<AppState>();
            let db = st.db.lock().map_err(|e| e.to_string())?;
            core_library::delete_snapshot(&db.conn, snapshot_id).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

// ---------- Merge to HDR ----------

/// Merge 2–9 bracketed RAW frames (tripod, manually selected) into one scene-referred HDR EXR,
/// write it into the library, catalog it (`format='hdr'`), and return the new row.
///
/// Pipeline: numeric EXIF → EV₁₀₀ per frame → reference = median EV → decode every frame full-res
/// with the REFERENCE frame's as-shot WB (`develop_linear_wb`) → streaming exposure-weighted merge
/// (`core_hdr::MergeAccumulator`) → fp16 linear-ProPhoto EXR with embedded metadata + parentage
/// (`core_raw::write_hdr_exr`) → `process_file` + `insert_image`.
///
/// Emits `hdr:progress {done, total, stage}` (total = frames + 2: each merged frame, the EXR
/// write, the catalog step) and `hdr:done {image}` + `library:changed` on success.
#[tauri::command]
pub async fn hdr_merge(app: AppHandle, image_ids: Vec<i64>) -> Result<ImageRow, String> {
    use std::sync::atomic::AtomicBool;

    crate::jobs::spawn_job(
        app.clone(),
        core_app::jobs::JobClass::Import,
        false,
        "hdr_merge",
        move |ctx| {
            let st = app.state::<AppState>();

            // Single-flight: a merge decodes N full-res RAWs; a second one would thrash memory.
            if st.hdr_running.swap(true, Ordering::SeqCst) {
                return Err("an HDR merge is already running".to_string());
            }
            st.workers.reset_cancel(&st.hdr_cancel)?;
            // Resets BOTH the running gate and the cancel flag on drop, so an early return / error can
            // never wedge the gate or leave a stale cancel latched for the next merge.
            struct Running<'a>(&'a AtomicBool, &'a AtomicBool);
            impl Drop for Running<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                    self.1.store(false, Ordering::SeqCst);
                }
            }
            let _running = Running(&st.hdr_running, &st.hdr_cancel);
            // Gate the FS watcher so it can't index the freshly-renamed EXR before our own catalog
            // step (insert is hash-idempotent, but the watcher path would race the returned row).
            let _watch_guard = crate::watch::ImportGuard::new(app.clone());
            // The user's stop flag, a shutdown, or a supervisor cancel all stop the job.
            let cancelled = || {
                st.workers.is_stopping()
                    || st.hdr_cancel.load(Ordering::SeqCst)
                    || ctx.is_cancel_requested()
            };

            // Dedupe while preserving selection order — a duplicate id would double-count a frame and
            // skew the exposure-weighted merge.
            let mut seen_ids = std::collections::HashSet::new();
            let image_ids: Vec<i64> = image_ids
                .into_iter()
                .filter(|id| seen_ids.insert(*id))
                .collect();

            if !(2..=9).contains(&image_ids.len()) {
                return Err(format!(
                    "select 2–9 bracketed RAW frames to merge (got {})",
                    image_ids.len()
                ));
            }
            let total = image_ids.len() + 2;
            let progress = |done: usize, stage: &str| {
                let _ = app.emit(
                    "hdr:progress",
                    serde_json::json!({"done": done, "total": total, "stage": stage}),
                );
            };

            // ONE snapshot of the source rows: every field used below (path, status, format,
            // hash, folder) and the tokens fencing the minutes of unlocked decode come from it, so
            // a replaced or rowid-reused source cannot slip between a read and the capture.
            struct Frame {
                id: i64,
                path: PathBuf,
                hash_hex: String,
                folder_id: i64,
            }
            let snapshot =
                crate::source_fence::snapshot_sources(&st.publications, &st.db, &image_ids)?;
            snapshot.require_present()?;
            if let Some(s) = snapshot.sources().iter().find(|s| s.format != "raw") {
                return Err(format!(
                    "{}: HDR merge takes RAW frames only (this is {})",
                    s.path.display(),
                    s.format
                ));
            }
            let tokens = snapshot.tokens().to_vec();
            let frames: Vec<Frame> = snapshot
                .sources()
                .iter()
                .map(|s| Frame {
                    id: s.id,
                    path: s.path.clone(),
                    hash_hex: s.content_hash_hex.clone(),
                    folder_id: s.folder_id.unwrap_or(-1),
                })
                .collect();

            // Numeric exposure per frame → EV, reference (median EV), shortest (max EV). Metadata-only
            // (no pixel decode) alongside, to catch a mixed-camera selection before the expensive
            // full-res decode loop below.
            // TODO(P4): auto-bracket suggestion — burst edges in pano_detect already absorb HDR
            // brackets; a varying-EV burst category can reuse that pipeline.
            progress(0, "reading exposure");
            type CamId = (Option<String>, Option<String>);
            let (exposures, cam_ids): (Vec<core_hdr::ExposureInfo>, Vec<CamId>) = frames
                .iter()
                .map(|f| {
                    let src = core_raw::source_from_path(&f.path)
                        .map_err(|e| format!("{}: {e}", f.path.display()))?;
                    let (t, n, iso) = core_raw::read_exposure_numeric(&src)
                        .map_err(|e| format!("{}: {e}", f.path.display()))?
                        .ok_or_else(|| {
                            format!(
                                "{}: missing shutter/aperture/ISO EXIF (needed for EV math)",
                                f.path.display()
                            )
                        })?;
                    let meta = core_raw::read_metadata(&src)
                        .map_err(|e| format!("{}: {e}", f.path.display()))?;
                    Ok((
                        core_hdr::ExposureInfo {
                            exposure_time_s: t,
                            f_number: n,
                            iso,
                        },
                        (meta.camera_make, meta.camera_model),
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?
                .into_iter()
                .unzip();
            // Refuse mixed-camera brackets up front: the merged EXR carries one reference frame's
            // metadata, so frames from different bodies would silently blend mismatched color/WB.
            let (first_make, first_model) = &cam_ids[0];
            for (make, model) in &cam_ids[1..] {
                if (make, model) != (first_make, first_model) {
                    return Err(format!(
                        "all frames must come from the same camera (found {} {} and {} {})",
                        first_make.as_deref().unwrap_or("?"),
                        first_model.as_deref().unwrap_or("?"),
                        make.as_deref().unwrap_or("?"),
                        model.as_deref().unwrap_or("?"),
                    ));
                }
            }
            let evs: Vec<f64> = exposures
                .iter()
                .map(|e| core_hdr::ev100(e).map_err(|e| e.to_string()))
                .collect::<Result<_, String>>()?;
            if evs.iter().all(|&ev| (ev - evs[0]).abs() < 0.2) {
                return Err(
                    "all frames have the same exposure — select a bracketed set (varying shutter)"
                        .to_string(),
                );
            }
            let ref_idx = core_hdr::reference_index(&exposures).map_err(|e| e.to_string())?;
            let shortest_idx = (0..evs.len())
                .max_by(|&a, &b| evs[a].total_cmp(&evs[b]))
                .expect("non-empty");

            // Decode the reference first (capturing its as-shot WB), then stream the rest. The reference
            // stays resident because each moving frame is aligned against its pixels; `with_reference`
            // clones it into the accumulator for deghosting, so the original is dropped only after the
            // alignment loop.
            let ref_src = core_raw::source_from_path(&frames[ref_idx].path)
                .map_err(|e| format!("{}: {e}", frames[ref_idx].path.display()))?;
            let (ref_img, ref_wb) =
                core_raw::develop_linear_wb(&ref_src, None).map_err(|e| e.to_string())?;
            let (ref_w, ref_h) = (ref_img.width, ref_img.height);
            let mut acc = core_hdr::MergeAccumulator::with_reference(
                &ref_img,
                ref_idx == shortest_idx,
                core_hdr::DeghostParams::default(),
            )
            .map_err(|e| e.to_string())?;
            let mut done = 1;
            // Frames the aligner couldn't confidently register — merged unaligned, surfaced to the user.
            let mut warnings: Vec<String> = Vec::new();
            progress(done, "merging");
            for (i, frame) in frames.iter().enumerate() {
                if i == ref_idx {
                    continue;
                }
                // Poll cancel between frames (each frame is a multi-second decode + align + warp).
                if cancelled() {
                    return Err("cancelled".to_string());
                }
                let src = core_raw::source_from_path(&frame.path)
                    .map_err(|e| format!("{}: {e}", frame.path.display()))?;
                let (img, _) = core_raw::develop_linear_wb(&src, Some(ref_wb))
                    .map_err(|e| format!("{}: {e}", frame.path.display()))?;
                let scale = core_hdr::relative_scale(&exposures[i], &exposures[ref_idx])
                    .map_err(|e| e.to_string())? as f32;

                // Hand-held alignment: recover the affine transform mapping this frame onto the
                // reference grid, warp it there (with a validity mask), and merge the aligned pixels.
                // If the pair won't register, fall back to an unaligned merge with a warning.
                progress(done, "aligning");
                let aligned = core_pano::align::estimate_alignment_rgb(
                    &ref_img.data,
                    ref_w as usize,
                    ref_h as usize,
                    &img.data,
                    img.width as usize,
                    img.height as usize,
                    core_pano::align::AlignModel::Affine,
                );
                progress(done, "merging");
                match aligned {
                    Some(m) => {
                        let (warped, mask) = core_hdr::warp_into_reference(&img, ref_w, ref_h, &m);
                        let warped_img = core_raw::LinearImage {
                            width: ref_w,
                            height: ref_h,
                            data: warped,
                        };
                        acc.add_frame_masked(&warped_img, scale, i == shortest_idx, &mask)
                            .map_err(|e| format!("{}: {e}", frame.path.display()))?;
                    }
                    None => {
                        warnings.push(format!(
                            "frame {} ({}) couldn't be aligned — merged unaligned",
                            i + 1,
                            frame
                                .path
                                .file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("?"),
                        ));
                        acc.add_frame(&img, scale, i == shortest_idx)
                            .map_err(|e| format!("{}: {e}", frame.path.display()))?;
                    }
                }
                done += 1;
                progress(done, "merging");
            }
            drop(ref_img);
            let merged = acc.finish().map_err(|e| e.to_string())?;

            // Destination: library_root/YYYY/YYYY-MM-DD (import's date routing) when a root is set,
            // else next to the reference frame. Metadata = the reference frame's (rides inside the EXR).
            let meta = core_raw::read_metadata(&ref_src).map_err(|e| e.to_string())?;
            let library_root: Option<String> = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::library_root(&db.conn).map_err(|e| e.to_string())?
            };
            let ref_parent = frames[ref_idx]
                .path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| "reference frame has no parent directory".to_string())?;
            let dest_dir = match &library_root {
                Some(root) => {
                    let capture = meta
                        .capture_date
                        .unwrap_or_else(core_library::index::now_epoch);
                    Path::new(root).join(core_import::date_subpath(capture))
                }
                None => ref_parent.clone(),
            };
            std::fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;
            let ref_stem = frames[ref_idx]
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("merge");
            let dest = core_import::unique_dest(&dest_dir, &format!("{ref_stem}_HDR.exr"));

            let sources: Vec<core_raw::HdrSourceInfo> = frames
                .iter()
                .enumerate()
                .map(|(i, f)| core_raw::HdrSourceInfo {
                    image_id: f.id,
                    content_hash_hex: f.hash_hex.clone(),
                    relative_ev: (evs[i] - evs[ref_idx]) as f32,
                })
                .collect();
            core_raw::write_hdr_exr(&dest, &merged, &meta, &sources, ref_idx)
                .map_err(|e| e.to_string())?;
            drop(merged);
            if cancelled() {
                let _ = std::fs::remove_file(&dest);
                return Err("cancelled".into());
            }
            done += 1;
            progress(done, "writing");

            // Catalog: hash + thumb + meta from the file (metadata flows from the EXR attributes),
            // then a brief lock to insert. Folder row: the library root when routing under it,
            // else the reference frame's own folder.
            let processed = core_library::process_file(&dest, &st.thumbs, core_library::THUMB_SIZE)
                .map_err(|e| {
                    // A written-but-uncatalogable EXR would sit orphaned in the library folder.
                    let _ = std::fs::remove_file(&dest);
                    e.to_string()
                })?;
            // Any catalog failure after a successful write would leave the EXR orphaned on disk —
            // remove it on every error path out of this block (the collision branch's own remove
            // stays; a second remove is a no-op).
            let id = crate::source_fence::publish_owned(
                &st.publications,
                &tokens,
                &dest,
                |guard| -> Result<i64, String> {
                    let db = st.db.lock().map_err(|e| e.to_string())?;
                    let conn = &db.conn;
                    let folder_id = match &library_root {
                        Some(root) => core_library::add_root(conn, Path::new(root))
                            .map_err(|e| e.to_string())?,
                        None if frames[ref_idx].folder_id >= 0 => frames[ref_idx].folder_id,
                        None => {
                            core_library::add_root(conn, &ref_parent).map_err(|e| e.to_string())?
                        }
                    };
                    let id = core_library::insert_hdr_image(
                        conn,
                        folder_id,
                        core_library::index::now_epoch(),
                        &processed,
                        &sources,
                    )
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| {
                        // Byte-identical EXR already catalogued (same bracket merged before).
                        "an identical merge of these frames is already in the library".to_string()
                    })?;
                    // Register the committed row's identity while the gate is still held: the
                    // rowid may be one retired by an earlier delete (`images.id` has no
                    // AUTOINCREMENT), and an unregistered reuse would stay Stale until restart.
                    guard.replace_source(
                        core_model::identity::ImageId(id),
                        core_runtime::publication::SourceKey {
                            hash: processed.content_hash_hex.clone(),
                            path: processed.path.clone(),
                        },
                    );
                    Ok(id)
                },
            )?;
            // Image + provenance committed; a later cancel must not retire the published row.
            ctx.mark_committed();
            // The image and its provenance are committed now. A response-query failure must not
            // remove the successfully published file and leave a dangling catalog row.
            let row = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                core_library::image_by_id(&db.conn, id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "merged image row vanished after insert".to_string())?
            };
            done += 1;
            progress(done, "done");
            let _ = app.emit(
                "hdr:done",
                serde_json::json!({ "image": &row, "warnings": warnings }),
            );
            let _ = app.emit("library:changed", ());
            crate::thumb_queue::enqueue_all(&app);
            Ok(row)
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

/// Request the running HDR merge to stop (honored between frames). No-op if idle.
#[tauri::command]
pub fn hdr_cancel(app: AppHandle) {
    let st = app.state::<AppState>();
    if st.hdr_running.load(Ordering::SeqCst) {
        st.hdr_cancel.store(true, Ordering::SeqCst);
    }
}

/// Export a merged-HDR image (`format='hdr'`) as an uncompressed FLOAT LinearRaw DNG for
/// Lightroom/ACR interop — the full >1.0 scene-referred headroom Merge-to-HDR produced survives
/// untouched (no bit-depth quantization). Export-only: the written DNG is never re-imported into the
/// catalog (Darkroom itself always reads the source `.exr`).
#[tauri::command]
pub async fn hdr_export_dng(app: AppHandle, image_id: i64, dest: String) -> Result<(), String> {
    crate::jobs::spawn_fallible(
        app.clone(),
        core_app::jobs::JobClass::Export,
        true,
        "hdr_export_dng",
        move || {
            let st = app.state::<AppState>();
            // Keep the watcher from cataloguing an existing export between the source-path check and
            // atomic replacement. A newly catalogued file would otherwise become a protected source.
            let _watch_guard = crate::watch::ImportGuard::new(app.clone());
            let (path, format) = {
                let db = st.db.lock().map_err(|e| e.to_string())?;
                db.conn
                    .query_row(
                        "SELECT path, IFNULL(format,'raw') FROM images WHERE id = ?1",
                        core_db::rusqlite::params![image_id],
                        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
                    )
                    .map_err(|_| format!("image {image_id} not found"))?
            };
            if format != "hdr" {
                return Err(format!(
                    "image {image_id} is not a merged-HDR image (format={format})"
                ));
            }
            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
            let lin = core_raw::hdr_file::read_hdr_linear(&bytes).map_err(|e| e.to_string())?;
            let meta = core_raw::hdr_file::read_hdr_meta(&bytes);
            let dest_path = Path::new(&dest);
            ensure_export_destination(st.inner(), dest_path)?;
            core_raw::write_hdr_dng(dest_path, &lin, &meta).map_err(|e| e.to_string())
        },
    )
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod preset_tests {
    /// The `core-preset` ModuleScope owns a copy of the DevelopParams field-key list (it must not
    /// depend on the GPU crate). This guards that copy against drift from the real struct.
    #[test]
    fn module_scope_covers_all_develop_params_fields() {
        let v = serde_json::to_value(core_pipeline::DevelopParams::default()).unwrap();
        let mut actual: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        actual.sort();
        let mut scoped: Vec<String> = core_preset::all_field_keys()
            .iter()
            .map(|s| s.to_string())
            .collect();
        scoped.sort();
        assert_eq!(
            actual, scoped,
            "core-preset ModuleScope drifted from DevelopParams fields"
        );
    }

    /// Every bundled built-in preset must be self-consistent: its `params` keys ⊆ `fieldKeys` ⊆ the
    /// real DevelopParams fields, and the sparse params must overlay onto defaults and deserialize
    /// into a typed `DevelopParams`. Catches a hand-edited typo in a `resources/presets/*.json`.
    #[test]
    fn builtin_presets_are_valid() {
        use std::collections::HashSet;
        let all: HashSet<String> = core_preset::all_field_keys()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let base = serde_json::to_value(core_pipeline::DevelopParams::default()).unwrap();
        for raw in super::BUILTIN_PRESETS {
            let env: super::PresetEnvelope =
                serde_json::from_str(raw).expect("built-in preset must parse");
            let obj = env
                .params
                .as_object()
                .unwrap_or_else(|| panic!("{}: params must be an object", env.name));
            for k in obj.keys() {
                assert!(
                    env.field_keys.contains(k),
                    "{}: param key '{k}' is not listed in fieldKeys",
                    env.name
                );
            }
            for k in &env.field_keys {
                assert!(
                    all.contains(k),
                    "{}: fieldKey '{k}' is not a real DevelopParams field",
                    env.name
                );
            }
            let merged = core_preset::apply_sparse(&base, obj, 1.0);
            serde_json::from_value::<core_pipeline::DevelopParams>(merged).unwrap_or_else(|e| {
                panic!(
                    "{}: params do not form valid develop settings: {e}",
                    env.name
                )
            });
        }
    }

    /// Applying a sparse preset must overlay ONLY its own fields — an untouched edit field survives.
    /// This is the core sparse-merge guarantee that lets a preset land on an already-edited image.
    #[test]
    fn applying_a_sparse_preset_leaves_untouched_fields_intact() {
        let edit = core_pipeline::DevelopParams {
            exposure: 1.5, // an existing user edit
            ..Default::default()
        };
        let base = serde_json::to_value(&edit).unwrap();
        let mut overlay = serde_json::Map::new();
        overlay.insert("contrast".into(), serde_json::json!(30.0)); // preset touches only contrast
        let merged = core_preset::apply_sparse(&base, &overlay, 1.0);
        let out: core_pipeline::DevelopParams = serde_json::from_value(merged).unwrap();
        assert_eq!(out.contrast, 30.0, "preset field must be applied");
        assert_eq!(out.exposure, 1.5, "untouched edit field must be preserved");
    }
}

/// These guard the non-destructive contract: a stored edit that no longer parses into the current
/// schema must never be silently overwritten by an auto-save — only by an explicit user Reset.
#[cfg(test)]
mod develop_edit_tests {
    use super::stored_edit_is_unreadable;
    use core_pipeline::DevelopParams;

    /// An image with no stored edit is the normal first-touch case — auto-save must proceed.
    #[test]
    fn no_stored_edit_is_not_unreadable() {
        assert!(!stored_edit_is_unreadable(None));
    }

    /// A blob written by this build round-trips, so it is safe to overwrite.
    #[test]
    fn a_valid_stored_edit_is_readable() {
        let json = serde_json::to_string(&DevelopParams::default()).unwrap();
        assert!(!stored_edit_is_unreadable(Some(&json)));
    }

    /// Truncated or non-JSON rows (interrupted write, disk corruption) must be protected.
    #[test]
    fn garbage_is_unreadable() {
        assert!(stored_edit_is_unreadable(Some("not json at all")));
        assert!(stored_edit_is_unreadable(Some("{")));
    }

    /// Well-formed JSON whose field types no longer match the struct (a breaking schema change)
    /// is just as unreadable — and just as destructive to clobber.
    #[test]
    fn a_wrong_typed_field_is_unreadable() {
        let mut v = serde_json::to_value(DevelopParams::default()).unwrap();
        let obj = v.as_object_mut().unwrap();
        assert!(
            obj.contains_key("exposure"),
            "DevelopParams no longer serializes an 'exposure' field"
        );
        obj.insert("exposure".into(), serde_json::json!("very bright"));
        let json = serde_json::to_string(&v).unwrap();
        assert!(stored_edit_is_unreadable(Some(&json)));
    }
}

#[cfg(test)]
mod export_tests {
    use super::{ensure_not_source, write_atomic, write_new_export};
    use std::path::PathBuf;

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dr-export-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The point of the `.part` dance: the destination is either absent or complete, and no debris
    /// is left beside it for a startup sweep to reason about.
    #[test]
    fn leaves_no_part_file_behind() {
        let dir = tempdir("ok");
        let dest = dir.join("photo.jpg");
        write_atomic(&dest, b"finished").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"finished");
        assert!(!dir.join("photo.jpg.part").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Re-exporting over an existing file must replace it, not fail on the rename.
    #[test]
    fn overwrites_an_existing_export() {
        let dir = tempdir("over");
        let dest = dir.join("photo.jpg");
        std::fs::write(&dest, b"old").unwrap();
        write_atomic(&dest, b"new").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An unwritable destination directory must surface as an error and leave nothing.
    #[test]
    fn a_failed_write_creates_nothing() {
        let dir = tempdir("fail");
        let dest = dir.join("missing-subdir").join("photo.jpg");
        assert!(write_atomic(&dest, b"x").is_err());
        assert!(!dest.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_protects_catalog_sources_and_allows_other_destinations() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.png");
        std::fs::write(&source, b"original").unwrap();
        let sources = vec![source.to_string_lossy().into_owned()];
        assert!(ensure_not_source(&source, &sources).is_err());
        assert!(ensure_not_source(&dir.path().join("./original.png"), &sources).is_err());
        assert!(ensure_not_source(&dir.path().join("export.png"), &sources).is_ok());
        assert_eq!(std::fs::read(&source).unwrap(), b"original");
    }

    #[cfg(unix)]
    #[test]
    fn export_protects_symlink_aliases_of_sources() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("original.png");
        let alias = dir.path().join("alias.png");
        std::fs::write(&source, b"original").unwrap();
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        assert!(ensure_not_source(&alias, &[source.to_string_lossy().into_owned()]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn export_protects_missing_sources_through_parent_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let original_dir = dir.path().join("originals");
        let alias_dir = dir.path().join("alias");
        std::fs::create_dir(&original_dir).unwrap();
        std::os::unix::fs::symlink(&original_dir, &alias_dir).unwrap();
        let source = original_dir.join("missing.dng");
        let sources = [source.to_string_lossy().into_owned()];
        assert!(ensure_not_source(&alias_dir.join("missing.dng"), &sources).is_err());
        assert!(
            ensure_not_source(&original_dir.join("../originals/missing.dng"), &sources).is_err()
        );
    }

    #[test]
    fn export_does_not_touch_an_unrelated_part_file() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        let other = dir.path().join("photo.jpg.part");
        std::fs::write(&other, b"another operation").unwrap();
        write_atomic(&dest, b"complete").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"complete");
        assert_eq!(std::fs::read(&other).unwrap(), b"another operation");
    }

    #[test]
    fn batch_export_never_replaces_an_existing_deliverable() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("photo.jpg");
        std::fs::write(&dest, b"previous export").unwrap();
        assert!(write_new_export(&dest, b"new pixels").is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous export");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
