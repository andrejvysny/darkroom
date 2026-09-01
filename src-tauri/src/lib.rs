mod backup;
mod commands;
mod events;
mod features;
mod logging;
mod model_mgmt;
mod pano_detect;
mod panorama;
mod prefetch;
mod protocol;
mod scan;
mod state;
mod suggest;
mod thumb_queue;
mod watch;

#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod analysis;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
#[path = "analysis_stub.rs"]
mod analysis;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod denoise;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
#[path = "denoise_stub.rs"]
mod denoise;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod faces;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
#[path = "faces_stub.rs"]
mod faces;
#[cfg(not(all(target_os = "macos", target_arch = "x86_64")))]
mod segment;
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
#[path = "segment_stub.rs"]
mod segment;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init());

    // Tier-3 E2E: real-backend UI automation via the tauri-plugin-playwright socket bridge.
    // Behind a feature + optional dep so it is never compiled into release builds.
    #[cfg(feature = "e2e-testing")]
    {
        builder = builder.plugin(tauri_plugin_playwright::init());
    }

    builder
        .register_asynchronous_uri_scheme_protocol("thumb", |ctx, req, responder| {
            protocol::handle_thumb(ctx, req, responder)
        })
        .setup(|app| {
            // Logging must never keep the app from starting — fall back to stderr-only.
            if let Err(e) = logging::init(app.handle()) {
                eprintln!("darkroom: file logging unavailable: {e}");
            }
            install_panic_hook();
            // Grant the playwright permission at runtime (debug-only `dynamic-acl`), so the
            // capability never lives in capabilities/ and feature-off builds stay clean.
            #[cfg(feature = "e2e-testing")]
            {
                app.handle()
                    .add_capability(include_str!("../e2e-capability.json"))?;
            }

            let state = match AppState::new(app.handle()) {
                Ok(state) => state,
                Err(msg) => fatal_startup_error(&msg),
            };
            app.manage(state);

            // Crash recovery: stamp `finished_at` on any import sessions a previous run left open
            // (killed/crashed mid-import). Best-effort; the per-file copies are already catalogued.
            {
                let st = app.state::<AppState>();
                let lock = st.db.lock();
                if let Ok(db) = lock {
                    let _ = core_library::reap_dangling_import_sessions(&db.conn);
                }
            }

            // Seed the bundled built-in develop presets (idempotent).
            {
                let st = app.state::<AppState>();
                let lock = st.db.lock();
                if let Ok(db) = lock {
                    commands::seed_builtin_presets(&db.conn);
                }
            }

            // Mark the start of a usage session in the behavioral-signal log (best-effort).
            {
                let st = app.state::<AppState>();
                crate::events::log_event(
                    st.inner(),
                    core_library::Event {
                        event_type: "session.start".into(),
                        ..Default::default()
                    },
                );
            }

            // Start the background canonical-thumbnail worker (parks until there's work).
            thumb_queue::spawn_worker(app.handle().clone());

            // Best-effort daily catalog backup (own background thread; logs its own outcome).
            backup::maybe_backup_on_startup(app.handle().clone());

            // Reconcile against disk, then start the FS watcher — off the setup thread so a slow
            // stat sweep can't delay window creation. The watcher is parked in AppState to stay alive.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                watch::reconcile_on_launch(&handle);
                if let Some(w) = watch::spawn_watcher(handle.clone()) {
                    let st = handle.state::<AppState>();
                    let lock = st.watcher.lock();
                    if let Ok(mut slot) = lock {
                        *slot = Some(w);
                    }
                }
                // Backfill canonical thumbnails for the whole library at low priority (visible /
                // just-opened images are promoted to the front via `thumb_prioritize`). After
                // reconcile so freshly re-added rows are included.
                thumb_queue::enqueue_all(&handle);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library_query,
            commands::library_count,
            commands::library_folders,
            commands::library_date_tree,
            commands::image_meta,
            commands::gpu_status,
            commands::library_index_root,
            commands::database_reset,
            commands::app_default_library,
            commands::develop_get_edit,
            commands::develop_set_edit,
            commands::develop_render,
            commands::develop_regen_thumb,
            commands::develop_preview_jpeg,
            commands::thumb_prioritize,
            commands::develop_session,
            commands::develop_prefetch,
            commands::develop_get_histogram,
            commands::develop_histogram,
            commands::develop_apply_settings,
            commands::presets_list,
            commands::presets_get,
            commands::presets_save,
            commands::presets_update,
            commands::presets_delete,
            commands::presets_duplicate,
            commands::presets_apply,
            commands::presets_export,
            commands::presets_import_file,
            commands::snapshots_list,
            commands::snapshot_create,
            commands::snapshot_restore,
            commands::snapshot_rename,
            commands::snapshot_delete,
            commands::export_image,
            commands::cull_set_rating,
            commands::cull_set_flag,
            commands::cull_set_label,
            commands::cull_set_rating_many,
            commands::cull_set_flag_many,
            commands::cull_set_label_many,
            commands::cull_rejected_summary,
            commands::cull_delete_rejected,
            commands::keywords_list,
            commands::keywords_for_image,
            commands::keyword_add_to_image,
            commands::keyword_add_to_images,
            commands::keyword_remove_from_image,
            commands::keyword_delete,
            commands::collections_list,
            commands::collections_for_image,
            commands::collection_create,
            commands::collection_rename,
            commands::collection_delete,
            commands::collection_add_images,
            commands::collection_remove_images,
            commands::app_library_root,
            commands::set_library_root,
            commands::dedup_scan,
            commands::dedup_scan_perceptual,
            commands::dedup_resolve,
            commands::dedup_resolve_bulk,
            commands::import_list,
            commands::import_dedup,
            commands::import_thumb,
            commands::import_commit,
            commands::hdr_merge,
            commands::hdr_cancel,
            commands::hdr_export_dng,
            commands::image_sources,
            commands::image_pair,
            commands::image_pair_unlink,
            commands::thumb_cache_cap,
            commands::thumb_cache_size,
            commands::set_thumb_cache_cap,
            commands::preview_edge,
            commands::set_preview_edge,
            commands::analysis_status,
            commands::analysis_models_ensure,
            commands::analysis_run,
            commands::analysis_cancel,
            commands::scan_scope_counts,
            commands::scan_run,
            commands::scan_cancel,
            commands::scan_running,
            commands::scan_prefs_get,
            commands::scan_prefs_set,
            commands::image_scan_state,
            commands::denoise_status,
            commands::denoise_apply,
            commands::denoise_clear,
            commands::denoise_cancel,
            commands::panorama_preview,
            commands::panorama_merge,
            commands::panorama_cancel,
            commands::panorama_status,
            commands::panorama_preview_release,
            commands::pano_detect_run,
            commands::pano_detect_cancel,
            commands::pano_detect_status,
            commands::pano_detect_groups,
            commands::pano_detect_dismiss,
            commands::pano_detect_mark_merged,
            commands::mask_ai_models_ensure,
            commands::mask_ai_ready,
            commands::mask_ai_encode,
            commands::mask_ai_prompt,
            commands::mask_ai_tier_get,
            commands::mask_ai_tier_set,
            commands::analysis_facets,
            commands::image_detections,
            commands::image_caption,
            commands::image_presence,
            commands::image_user_labels,
            commands::set_image_user_label,
            commands::set_image_user_label_many,
            commands::analysis_detector_size,
            commands::set_analysis_detector_size,
            commands::models_overview,
            commands::models_cancel,
            commands::models_remove,
            commands::faces_status,
            commands::faces_models_ensure,
            commands::faces_run,
            commands::faces_cancel,
            commands::face_stage_enabled,
            commands::set_face_stage_enabled,
            commands::people_list,
            commands::person_faces,
            commands::image_faces,
            commands::person_set_name,
            commands::person_set_hidden,
            commands::person_set_cover,
            commands::person_merge,
            commands::face_confirm,
            commands::face_reject,
            commands::face_assign,
            commands::faces_delete_all,
            commands::features_backfill,
            commands::suggest_train,
            commands::suggest_status,
            commands::sidecars_write_all,
            commands::sidecars_rebuild,
            commands::image_histogram,
            commands::frontend_log,
            commands::logs_status,
            commands::set_logs_directory,
            commands::set_log_level,
            commands::logs_export_zip,
            commands::logs_delete_all,
            commands::catalog_backup_now,
            commands::catalog_backup_status,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Flush the WAL into the main catalog file on quit so recent rows aren't stranded in
            // `catalog.db-wal` (and a later corrupt-check sees a consistent file). Best-effort.
            if let tauri::RunEvent::Exit = event {
                let st = app_handle.state::<AppState>();
                let lock = st.db.lock();
                if let Ok(db) = lock {
                    let _ = db.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                }
            }
        });
}

/// Show a native error dialog, then exit cleanly. Returning `Err` from the setup hook instead
/// would panic inside the AppKit/Win32 launch callback and abort the process — the user sees only
/// "Darkroom quit unexpectedly" with no explanation (exactly how the 0.1.1 schema-guard refusal
/// presented). A blocking modal here is safe: setup runs on the main thread before the window
/// shows, the same stage where macOS itself presents launch modals.
fn fatal_startup_error(msg: &str) -> ! {
    tracing::error!(%msg, "startup failed");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Darkroom cannot start")
        .set_description(msg)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1)
}

/// Route panics into the tracing log (in addition to the default stderr hook), so a crash on a
/// background thread — whose stderr nobody is watching in a packaged app — still leaves a trace
/// in `darkroom.log`. Chains to the previous hook so default behavior (stderr message) is kept.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(%location, %payload, %backtrace, "panic");
        previous(info);
    }));
}
