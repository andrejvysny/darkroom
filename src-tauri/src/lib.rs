mod analysis;
mod commands;
mod events;
mod features;
mod protocol;
mod state;
mod watch;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init());

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
            // Grant the playwright permission at runtime (debug-only `dynamic-acl`), so the
            // capability never lives in capabilities/ and feature-off builds stay clean.
            #[cfg(feature = "e2e-testing")]
            {
                app.handle()
                    .add_capability(include_str!("../e2e-capability.json"))?;
            }

            let state = AppState::new(app.handle()).map_err(std::io::Error::other)?;
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
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library_query,
            commands::library_count,
            commands::library_folders,
            commands::image_meta,
            commands::library_index_root,
            commands::database_reset,
            commands::app_default_library,
            commands::develop_get_edit,
            commands::develop_set_edit,
            commands::develop_render,
            commands::develop_regen_thumb,
            commands::develop_preview_jpeg,
            commands::loupe_jpeg,
            commands::develop_get_histogram,
            commands::export_image,
            commands::cull_set_rating,
            commands::cull_set_flag,
            commands::cull_set_label,
            commands::cull_set_rating_many,
            commands::cull_set_flag_many,
            commands::cull_set_label_many,
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
            commands::dedup_scan,
            commands::dedup_scan_perceptual,
            commands::dedup_resolve,
            commands::dedup_resolve_bulk,
            commands::import_start,
            commands::thumb_cache_cap,
            commands::thumb_cache_size,
            commands::set_thumb_cache_cap,
            commands::analysis_status,
            commands::analysis_models_ensure,
            commands::analysis_run,
            commands::analysis_cancel,
            commands::analysis_facets,
            commands::image_detections,
            commands::image_caption,
            commands::image_presence,
            commands::image_user_labels,
            commands::set_image_user_label,
            commands::set_image_user_label_many,
            commands::analysis_detector_size,
            commands::set_analysis_detector_size,
            commands::features_backfill,
            commands::sidecars_write_all,
            commands::sidecars_rebuild,
            commands::image_histogram,
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
