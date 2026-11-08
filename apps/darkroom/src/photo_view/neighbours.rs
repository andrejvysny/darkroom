//! Neighbour selection for the photo worker's prefetch: the rows around the selected one in the
//! library order.

use super::PhotoView;
use crate::photo_worker::WarmRequest;
use core_app::state::ApplicationSnapshot;
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;

/// The first row `missing` does not reject, as a warm-up (rows without a path cannot decode).
fn nearest_present<'a>(
    mut rows: impl Iterator<Item = &'a ImageRow>,
    missing: &mut dyn FnMut(&ImageRow) -> bool,
) -> Option<WarmRequest> {
    rows.find(|row| !missing(row))
        .filter(|row| !row.path.is_empty())
        .map(|row| WarmRequest {
            image_id: row.id,
            path: std::path::PathBuf::from(&row.path),
            content_hash: row.content_hash.clone(),
        })
}

impl PhotoView {
    /// The rows right before and after the selected one in the library order, for the neighbour
    /// prefetch. At most 2, once per selected source. `missing` marks rows whose original is
    /// known to be gone; they are skipped in favour of the next row in that direction.
    pub fn prefetch_request(
        &mut self,
        snapshot: &ApplicationSnapshot,
        missing: &mut dyn FnMut(&ImageRow) -> bool,
    ) -> Option<Vec<WarmRequest>> {
        if !snapshot.catalog_ready || self.unavailable.is_some() {
            return None;
        }
        let image = snapshot.editor.image.as_ref()?;
        if snapshot.editor.selected != Some(ImageId(image.id)) {
            return None;
        }
        let source = (image.id, image.content_hash.clone());
        if self.prefetched_for.as_ref() == Some(&source) {
            return None;
        }
        let rows = &snapshot.library.rows;
        let at = rows.iter().position(|row| row.id == image.id)?;
        let next = nearest_present(rows[at + 1..].iter(), missing);
        let previous = nearest_present(rows[..at].iter().rev(), missing);
        self.prefetched_for = Some(source);
        // Next first: forward browsing is the common direction.
        Some(next.into_iter().chain(previous).collect())
    }
}
