//! `thumb://localhost/<content_hash_hex>?size=N` — streams cached thumbnail JPEG bytes.

use crate::state::AppState;
use core_library::{ThumbCache, THUMB_SIZE};
use tauri::http::{Request, Response};
use tauri::{Manager, Runtime, UriSchemeContext, UriSchemeResponder};

fn query_value<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|kv| kv.strip_prefix(key))
}

fn parse_size(query: Option<&str>) -> u32 {
    query_value(query, "size=")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(THUMB_SIZE)
}

/// Read the thumbnail. When `edit=<version>` is present the edit-rendered variant is served (it
/// versions the URL so the browser refetches after an edit); falls back to the base thumbnail if
/// the edited variant is missing. Otherwise the requested size, falling back to the default size.
fn read_thumb(thumbs: &ThumbCache, hash: &str, size: u32, edit: Option<i64>) -> Option<Vec<u8>> {
    // Validate hash to prevent path traversal — must be a 64-char hex digest.
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if let Some(version) = edit {
        if let Ok(bytes) = thumbs.read_edited(hash, version) {
            return Some(bytes);
        }
    }
    thumbs
        .read(hash, size)
        .ok()
        .or_else(|| thumbs.read(hash, THUMB_SIZE).ok())
}

pub fn handle_thumb<R: Runtime>(
    ctx: UriSchemeContext<'_, R>,
    request: Request<Vec<u8>>,
    responder: UriSchemeResponder,
) {
    let app = ctx.app_handle().clone();
    let uri = request.uri();
    let hash = uri.path().trim_start_matches('/').to_string();
    let size = parse_size(uri.query());
    let edit = query_value(uri.query(), "edit=").and_then(|v| v.parse::<i64>().ok());

    let state = app.state::<AppState>();
    let resp = match read_thumb(&state.thumbs, &hash, size, edit) {
        Some(bytes) => Response::builder()
            .status(200)
            .header("Content-Type", "image/jpeg")
            .header("Cache-Control", "public, max-age=31536000, immutable")
            .body(bytes)
            .unwrap_or_else(|_| Response::new(Vec::new())),
        None => Response::builder()
            .status(404)
            .body(Vec::new())
            .unwrap_or_else(|_| Response::new(Vec::new())),
    };
    responder.respond(resp);
}
