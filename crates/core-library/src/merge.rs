//! Catalog publication for a generated HDR image and its provenance.

use crate::{insert_image, LibError, ProcessedImage};
use core_db::rusqlite::{params, Connection};
use core_raw::HdrSourceInfo;

/// Insert an HDR image and all source links in one transaction. A missing/deleted source or any
/// other link failure must roll back the image too: the caller removes its generated file on error.
/// `None` means a byte-identical image already exists; its provenance is left unchanged.
pub fn insert_hdr_image(
    conn: &Connection,
    folder_id: i64,
    imported_at: i64,
    processed: &ProcessedImage,
    sources: &[HdrSourceInfo],
) -> Result<Option<i64>, LibError> {
    let tx = conn.unchecked_transaction()?;
    let Some(id) = insert_image(&tx, folder_id, imported_at, processed)? else {
        return Ok(None);
    };
    for (position, source) in sources.iter().enumerate() {
        if !source.relative_ev.is_finite() {
            return Err(LibError::Other("HDR source EV must be finite".into()));
        }
        tx.execute(
            "INSERT INTO hdr_sources (hdr_image_id, source_image_id, position, relative_ev)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, source.image_id, position as i64, source.relative_ev],
        )?;
    }
    tx.commit()?;
    Ok(Some(id))
}
