//! UI-side ownership of the presented frame.
//!
//! ([`Presenter::offer`]) is the single publication gate: a finished render becomes "the frame on
//! screen" only if it still matches what the UI asked for (image, catalog epoch, editor session,
//! selection generation, process version, renderer version, cache version, display revision,
//! device epoch, viewport) and is not older than what is already presented *for the same subject*
//! (a new subject's revisions start fresh, so its numbers are never compared with an old
//! subject's). Rejected leases are dropped, returning their texture to the pool.

use core_model::identity::{DeviceEpoch, FrameKey, QualityTier, RenderIdentity};

/// Outcome of a [`Presenter::offer`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Offer {
    /// The lease is now the frame on screen.
    Presented,
    /// The lease was rejected; it is dropped (texture back to the pool).
    Stale(StaleReason),
}

/// Why a finished render was not publishable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaleReason {
    /// Matches identity/viewport but is not the frame the UI wants (e.g. a `Settled` frame for a
    /// revision other than the wanted one).
    NotWanted,
    /// A same-subject, same-viewport already-presented frame is at least as new: an older
    /// `edit_revision` never replaces a newer one, and an `Interactive` frame never replaces the
    /// `Settled` frame of its own revision. Never compared across subjects.
    OlderRevision,
    /// Different image, catalog epoch, editor session, selection generation, process version,
    /// renderer version, cache version or display revision than the wanted frame.
    OtherImageOrSession,
    /// The frame was rendered on a retired GPU device.
    DeviceRetired,
    /// Viewport pixels, scale, or viewport revision differ from the wanted frame.
    ViewportChanged,
}

/// Owner of "what is on screen". The UI/coordinator calls [`Self::want`] with its newest request;
/// render workers call [`Self::offer`] when their render finishes, passing the *current* device
/// epoch (bumped when the host retires the device).
#[derive(Default)]
pub struct Presenter {
    current: Option<crate::FrameLease>,
    wanted: Option<FrameKey>,
}

impl Presenter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The newest request from the UI/coordinator.
    ///
    /// Switching to a *different subject* (other image, catalog epoch, editor session,
    /// selection generation, process version, renderer version, cache version or display
    /// revision) — or to a frame on a different device epoch — immediately drops the current
    /// frame: its pixels belong to the old subject or a retired device, so keeping them would
    /// show the previous photo under the new selection. A pure viewport change for the *same*
    /// subject deliberately keeps the current frame on screen: it is still the right photo,
    /// stretched or letterboxed by the host until the render for the new geometry lands.
    pub fn want(&mut self, key: FrameKey) {
        let keep_current = match &self.current {
            Some(current) => {
                let ck = current.key();
                same_subject(&ck.identity, &key.identity)
                    && ck.identity.device_epoch == key.identity.device_epoch
            }
            None => true,
        };
        if !keep_current {
            self.current = None; // lease dropped, texture back to the pool
        }
        self.wanted = Some(key);
    }

    /// The frame currently on screen.
    pub fn current(&self) -> Option<&crate::FrameLease> {
        self.current.as_ref()
    }

    /// Session disposal / device loss: drop the presented frame (texture returns to the pool).
    pub fn clear(&mut self) {
        self.current = None;
    }

    /// Publish a finished render if it is still valid to show. See [`StaleReason`] for the
    /// checks, applied in this order. An `Interactive` frame may present while a newer edit is
    /// pending (progress during drags); a `Settled` frame presents only for the wanted revision,
    /// replacing an Interactive one of the same revision. The reverse never happens: an
    /// `Interactive` frame of a revision that already settled is a straggler of that edit, not
    /// progress, and is rejected. The "older never replaces newer" revision comparison applies
    /// only within one subject and viewport geometry: a frame of a new subject is never compared
    /// against an old subject's revision (new editor sessions restart their revisions at 1), and
    /// frames of different viewport geometry are separate renders of the same photo, not a
    /// revision race.
    pub fn offer(&mut self, lease: crate::FrameLease, device_epoch: DeviceEpoch) -> Offer {
        let reason = self.stale_reason(&lease, device_epoch);
        if let Some(reason) = reason {
            drop(lease);
            return Offer::Stale(reason);
        }
        self.current = Some(lease);
        Offer::Presented
    }

    fn stale_reason(
        &self,
        lease: &crate::FrameLease,
        device_epoch: DeviceEpoch,
    ) -> Option<StaleReason> {
        let key = lease.key();
        // 1. Rendered on a retired device.
        if key.identity.device_epoch != device_epoch {
            return Some(StaleReason::DeviceRetired);
        }
        if let Some(wanted) = &self.wanted {
            // 2. Rendered on the wanted frame's (current) device.
            if key.identity.device_epoch != wanted.identity.device_epoch {
                return Some(StaleReason::DeviceRetired);
            }
            // 3. Same subject as the wanted frame.
            if !same_subject(&key.identity, &wanted.identity) {
                return Some(StaleReason::OtherImageOrSession);
            }
            // 4. Same viewport geometry as the wanted frame.
            if key.viewport_px != wanted.viewport_px
                || key.scale_milli != wanted.scale_milli
                || key.identity.viewport_revision != wanted.identity.viewport_revision
            {
                return Some(StaleReason::ViewportChanged);
            }
        }
        // 5. Never regress a presented frame — but only against a current frame of the *same
        //    subject and viewport geometry*. A new subject's revisions start fresh (a new editor
        //    session begins at 1), so its low revision must never be compared with an old
        //    subject's frame; cross-subject frames are gated by rule 3 against the wanted key,
        //    and `want` already dropped the old subject's current. Different viewport geometry
        //    means a different render of the same photo, not a revision race.
        if let Some(current) = &self.current {
            let ck = current.key();
            let comparable = same_subject(&key.identity, &ck.identity)
                && key.identity.device_epoch == ck.identity.device_epoch
                && key.viewport_px == ck.viewport_px
                && key.scale_milli == ck.scale_milli
                && key.identity.viewport_revision == ck.identity.viewport_revision;
            if comparable {
                if key.identity.edit_revision < ck.identity.edit_revision {
                    return Some(StaleReason::OlderRevision);
                }
                // A drag frame of the revision that has already *settled* is a straggler of the
                // very edit it led up to, not progress: the settled pixels are the final ones for
                // that revision. Publishing it would drop the settled lease, hand its texture to
                // the pool to be overwritten, and leave the drag approximation on screen until
                // the next edit — with nothing left to recover the settled frame. A *newer*
                // interactive revision is a genuinely later edit and still presents.
                if key.quality == QualityTier::Interactive
                    && ck.quality == QualityTier::Settled
                    && key.identity.edit_revision == ck.identity.edit_revision
                {
                    return Some(StaleReason::OlderRevision);
                }
            }
        }
        // 6. Quality/revision policy against the wanted frame.
        if let Some(wanted) = &self.wanted {
            // Interactive: progress during drags may be older than the wanted revision.
            // Settled: only the final-quality render of the wanted revision itself.
            if key.quality == QualityTier::Settled
                && key.identity.edit_revision != wanted.identity.edit_revision
            {
                return Some(StaleReason::NotWanted);
            }
        }
        None
    }
}

/// Everything except the edit revision (drag-progress) and the viewport fields (checked
/// separately) that identifies *what* is being rendered.
///
/// Every [`RenderIdentity`] field but `edit_revision` and the viewport fields belongs here: two
/// frames are interchangeable only if their identities are equal (`core_model::identity`), so a
/// bumped `renderer_version` (shader/generation change) or `cache_version` (a different
/// downstream cache generation) is a different subject — its pixels came from a different
/// renderer or cache generation than the one the UI asked for and must never present as though
/// they were current.
fn same_subject(a: &RenderIdentity, b: &RenderIdentity) -> bool {
    a.source.catalog_epoch == b.source.catalog_epoch
        && a.source.image_id == b.source.image_id
        && a.source.source_revision == b.source.source_revision
        && a.editor_session == b.editor_session
        && a.selection_generation == b.selection_generation
        && a.process_version == b.process_version
        && a.renderer_version == b.renderer_version
        && a.cache_version == b.cache_version
        && a.display_revision == b.display_revision
}
