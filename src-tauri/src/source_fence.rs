//! Publication fence for merge inputs: HDR/panorama provenance rows must attach to the exact
//! files the job read, not to a rowid that SQLite handed out again after a cull/dedup delete.

use core_model::identity::ImageId;
use core_runtime::publication::{PublicationToken, SourceKey};

use crate::state::AppState;

/// Captures publication tokens for merge inputs, so the final commit can prove every source
/// is still the same file it read (guards provenance against rowid reuse and catalog reset).
pub fn capture_sources(st: &AppState, ids: &[i64]) -> Result<Vec<PublicationToken>, String> {
    // Snapshot the sequence BEFORE reading rows: any source mutation between the read and
    // `capture_at` then fails the capture instead of vouching for a deleted/reused rowid.
    let sequence = st.publications.sequence();
    let sources = {
        let db = st.db.lock().map_err(|e| e.to_string())?;
        ids.iter()
            .map(|&id| {
                let (path, status, hash): (String, String, Vec<u8>) = db
                    .conn
                    .query_row(
                        "SELECT path, status, content_hash FROM images WHERE id = ?1",
                        core_db::rusqlite::params![id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .map_err(|_| format!("image {id} not found"))?;
                if status != "present" {
                    return Err(format!("{path}: file is missing — relink it first"));
                }
                let hash = if hash.len() == 32 {
                    let mut a = [0u8; 32];
                    a.copy_from_slice(&hash);
                    core_raw::hex(&a)
                } else {
                    String::new()
                };
                Ok((id, SourceKey { hash, path }))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    sources
        .into_iter()
        .map(|(id, key)| {
            st.publications
                .capture_at(ImageId(id), key, sequence)
                .map_err(|_| "sources changed while preparing the merge; try again".to_string())
        })
        .collect()
}
