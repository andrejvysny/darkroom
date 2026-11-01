//! Host binding between a core-import run and the job + catalog generation it started on:
//! cancellation/shutdown via [`ImportControl::should_stop`], catalog writes fenced on the epoch
//! that was current when the run began.

use crate::state::AppState;
use core_import::{ImportControl, SourceEvents};
use core_model::identity::{CatalogEpoch, ImageId};
use core_runtime::jobs::JobContext;

/// Binds a core-import run to its job and to the catalog generation it started on.
pub struct HostImportControl<'a> {
    st: &'a AppState,
    ctx: &'a JobContext,
    epoch: CatalogEpoch,
}

impl<'a> HostImportControl<'a> {
    /// Captures the current catalog epoch — call before the run touches the catalog.
    pub fn new(st: &'a AppState, ctx: &'a JobContext) -> Self {
        Self {
            st,
            ctx,
            epoch: st.publications.epoch(),
        }
    }

    /// The generation this run read; later catalog writes must be fenced on it.
    pub fn epoch(&self) -> CatalogEpoch {
        self.epoch
    }
}

impl ImportControl for HostImportControl<'_> {
    fn should_stop(&self) -> bool {
        self.ctx.is_cancel_requested() || self.st.workers.is_stopping()
    }

    fn committed(&self) {
        self.ctx.mark_committed();
    }

    fn commit(&self, commit: &mut dyn FnMut(&mut dyn SourceEvents)) -> bool {
        // Lock order: publication gate before the catalog (see `PublicationGate`).
        let mut guard = match self.st.publications.commit_guard(self.epoch) {
            Ok(guard) => guard,
            Err(_) => return false,
        };
        commit(&mut Events(&mut guard));
        true
    }
}

/// Forwards the source-identity changes of one fenced commit into the publication gate's source
/// index — relinks AND fresh inserts (a fresh insert may reuse a deleted row's id).
struct Events<'g, 'a>(&'g mut core_runtime::publication::SourceMutationGuard<'a>);

impl SourceEvents for Events<'_, '_> {
    fn source_changed(&mut self, image_id: i64, content_hash_hex: &str, path: &str) {
        self.0.replace_source(
            ImageId(image_id),
            core_runtime::publication::SourceKey {
                hash: content_hash_hex.to_string(),
                path: path.to_string(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str) -> core_runtime::publication::SourceKey {
        core_runtime::publication::SourceKey {
            hash: "ab".to_string(),
            path: path.to_string(),
        }
    }

    /// A source change recorded inside the fence is the identity later publication must match.
    #[test]
    fn source_change_replaces_the_fenced_source_identity() {
        let gate = core_runtime::publication::PublicationGate::default();
        let id = ImageId(7);
        {
            let mut guard = gate.commit_guard(gate.epoch()).expect("current epoch");
            Events(&mut guard).source_changed(id.0, "ab", "/lib/new.cr3");
        }
        assert!(gate.capture(id, key("/lib/new.cr3")).is_ok());
        assert!(gate.capture(id, key("/card/old.cr3")).is_err());
    }
}
