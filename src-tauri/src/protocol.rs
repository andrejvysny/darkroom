//! `thumb://localhost/<content_hash_hex>?size=N[&edit=V][&pv=1&edge=E]` — streams cached thumbnail
//! JPEG bytes. `pv=1&edge=E` requests the display-sharp preview tier (loupe / develop first-paint);
//! otherwise the small thumb tier (grid / filmstrip).

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

/// Read a cached render, preferring the unified pipeline source so every surface matches the editor.
///
/// When `preview` (the `pv=1&edge=E` loupe/develop request): edited preview → canonical preview →
/// fall through to the thumb tier (a soft placeholder until the preview renders). Otherwise the thumb
/// tier: edited variant → canonical `_dev<PV>` → camera placeholder at `size` → default size. The
/// canonical tiers are size-agnostic; `size` only selects among camera placeholders.
fn read_thumb(
    thumbs: &ThumbCache,
    hash: &str,
    size: u32,
    edit: Option<i64>,
    preview: bool,
    edge: u32,
) -> Option<Vec<u8>> {
    // Validate hash to prevent path traversal — must be a 64-char hex digest.
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let pv = crate::commands::PROCESS_VERSION;
    if preview && edge > 0 {
        match edit {
            // Edited image: only its OWN preview is valid. If it isn't rendered yet, fall through to
            // the edited thumb tier below — NOT the canonical preview (that's the unedited look).
            Some(version) => {
                if let Ok(bytes) = thumbs.read_edited_preview(hash, version, edge) {
                    return Some(bytes);
                }
            }
            None => {
                if let Ok(bytes) = thumbs.read_preview(hash, pv, edge) {
                    return Some(bytes);
                }
            }
        }
        // Preview not rendered yet — fall through to the (smaller) thumb tier as a placeholder.
    }
    if let Some(version) = edit {
        if let Ok(bytes) = thumbs.read_edited(hash, version) {
            return Some(bytes);
        }
    }
    thumbs
        .read_canonical(hash, pv)
        .ok()
        .or_else(|| thumbs.read(hash, size).ok())
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
    let preview = query_value(uri.query(), "pv=") == Some("1");
    let edge = query_value(uri.query(), "edge=")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    let state = app.state::<AppState>();
    let resp = match read_thumb(&state.thumbs, &hash, size, edit, preview, edge) {
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
