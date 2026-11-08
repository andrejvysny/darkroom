use crate::registry::ActionId;
use crate::state::ApplicationSnapshot;
use crate::AppError;
use core_model::identity::ImageId;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
    Initialized,
    JobsChanged,
    SavesDispatched,
    /// A close/replace/shutdown flush gave up waiting (see `BARRIER_TIMEOUT_MS`).
    BarrierTimedOut,
    /// The user discarded this image's unsaved edits.
    UnsavedEditsDiscarded(ImageId),
    /// A resolved preset / paste / restore was not applied: the document changed or closed while
    /// it was being resolved. Not an error; nothing changed.
    SettingsDropped,
    CommandAccepted(ActionId),
    EffectCompleted {
        action: ActionId,
        result: Result<(), AppError>,
    },
}

#[derive(Clone, Debug)]
pub struct SnapshotUpdate {
    pub revision: u64,
    pub at_millis: u64,
    pub notice: Notice,
    pub snapshot: Arc<ApplicationSnapshot>,
}
#[derive(Default)]
pub(crate) struct Slot {
    pub update: Option<SnapshotUpdate>,
    pub closed: bool,
}
pub struct Subscription {
    pub(crate) slot: Arc<Mutex<Slot>>,
}
impl Subscription {
    pub fn take_latest(&self) -> Option<SnapshotUpdate> {
        self.slot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .update
            .take()
    }
    pub fn close(&self) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.closed = true;
        slot.update = None;
    }
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.close();
    }
}
