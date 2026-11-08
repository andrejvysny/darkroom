//! In-flight continuous drag: one undo entry per gesture, updates stay out of history.

use std::sync::Arc;

use core_model::DevelopParams;

/// Pre-gesture snapshot; `Arc` clone makes begin free for large mask-heavy params.
#[derive(Debug, Clone)]
pub(crate) struct Gesture {
    pub(crate) before: Arc<DevelopParams>,
}

impl Gesture {
    pub(crate) fn begin(params: &Arc<DevelopParams>) -> Self {
        Self {
            before: params.clone(),
        }
    }
}
