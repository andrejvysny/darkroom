//! Culling marks (rating / flag / colour label): validation, the ordered effect and the snapshot
//! row patch applied when the catalog acknowledges the write.
use super::{ApplicationSession, BarrierKind};
use crate::workflow::{CullChange, Effect, Operation};
use crate::AppError;
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use std::sync::Arc;

const MAX_STARS: u8 = 5;

fn patch(row: &mut ImageRow, change: CullChange) {
    match change {
        CullChange::Rating(stars) => row.stars = i64::from(stars),
        CullChange::Flag(flag) => row.flag = flag.as_str().to_string(),
        CullChange::Label(label) => row.color_label = label.map(|l| l.as_str().to_string()),
    }
}

impl ApplicationSession {
    pub(super) fn set_cull(
        &mut self,
        image: ImageId,
        change: CullChange,
    ) -> Result<Vec<Effect>, AppError> {
        if image.0 <= 0 {
            return Err(AppError::InvalidCommand("image id must be positive".into()));
        }
        if matches!(change, CullChange::Rating(stars) if stars > MAX_STARS) {
            return Err(AppError::InvalidCommand("rating must be 0..=5".into()));
        }
        if matches!(&self.barrier, Some(b) if matches!(b.kind, BarrierKind::Shutdown)) {
            return Err(AppError::EditRefused(super::edit::QUITTING.into()));
        }
        // Never coalesced or replaced: every mark is its own ordered effect.
        Ok(vec![self.issue(Operation::SetCull {
            image_id: image,
            change,
        })])
    }

    pub(super) fn cull_applied(&mut self, image_id: ImageId, change: CullChange) {
        let s = Arc::make_mut(&mut self.snapshot);
        if s.library.rows.iter().any(|row| row.id == image_id.0) {
            let rows = Arc::make_mut(&mut s.library.rows);
            if let Some(row) = rows.iter_mut().find(|row| row.id == image_id.0) {
                patch(row, change);
            }
        }
        if let Some(row) = s.editor.image.as_mut().filter(|row| row.id == image_id.0) {
            patch(row, change);
        }
    }
}
