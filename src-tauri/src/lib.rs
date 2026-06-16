mod commands;
mod protocol;
mod state;

use state::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("thumb", |ctx, req, responder| {
            protocol::handle_thumb(ctx, req, responder)
        })
        .setup(|app| {
            let state = AppState::new(app.handle()).map_err(std::io::Error::other)?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::library_query,
            commands::library_count,
            commands::library_folders,
            commands::image_meta,
            commands::library_index_root,
            commands::app_default_library,
            commands::develop_get_edit,
            commands::develop_set_edit,
            commands::develop_render,
            commands::export_image,
            commands::cull_set_rating,
            commands::cull_set_flag,
            commands::cull_set_label,
            commands::keywords_list,
            commands::keywords_for_image,
            commands::keyword_add_to_image,
            commands::keyword_remove_from_image,
            commands::keyword_delete,
            commands::app_library_root,
            commands::dedup_scan,
            commands::dedup_resolve,
            commands::import_start,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
