//! Preset / copied-settings / snapshot application: resolve a source against the open document on
//! the catalog lane, then land the result as one undoable edit only if the document is unchanged.
use super::edit::require_finite;
use super::ApplicationSession;
use crate::editor::{EditorCommand, EditorState};
use crate::workflow::{Effect, Operation, SettingsFence, SettingsSource};
use crate::AppError;
use core_model::identity::ImageId;
use core_model::DevelopParams;
use std::sync::Arc;

fn invalid(message: &str) -> AppError {
    AppError::InvalidCommand(message.into())
}

fn check_source(source: &SettingsSource) -> Result<(), AppError> {
    match source {
        SettingsSource::Preset(id) | SettingsSource::Snapshot(id) if *id <= 0 => {
            Err(invalid("id must be positive"))
        }
        SettingsSource::Copied { params, .. } if !core_model::document::validate_finite(params) => {
            Err(AppError::EditRefused("non-finite edit value".into()))
        }
        _ => Ok(()),
    }
}

impl ApplicationSession {
    pub(super) fn load_snapshots(&mut self, image: ImageId) -> Result<Vec<Effect>, AppError> {
        if image.0 <= 0 {
            return Err(invalid("image id must be positive"));
        }
        Ok(vec![self.replace(Operation::LoadSnapshots(image))])
    }

    pub(super) fn apply_settings(
        &mut self,
        source: SettingsSource,
        amount: f32,
        replace_all: bool,
    ) -> Result<Vec<Effect>, AppError> {
        if !(0.0..=1.0).contains(&amount) {
            return Err(invalid("amount must be within 0..=1"));
        }
        check_source(&source)?;
        let (key, session) = self.edit_target()?;
        let EditorState::Ready(doc) = self.editor.state() else {
            return Err(AppError::EditRefused("no editable document".into()));
        };
        let fence = SettingsFence {
            image: key.image_id,
            session,
            revision: doc.revision,
        };
        let base = doc.params.clone();
        // Never coalesced: two quick applies each resolve against their own fence.
        Ok(vec![self.issue(Operation::ResolveSettings {
            fence,
            base,
            source,
            amount,
            replace_all,
        })])
    }

    /// Land a resolved result. `false` means the document moved on (or closed) since the fence and
    /// nothing changed; a refusal at apply time is reported through the snapshot's error.
    pub(super) fn settings_resolved(
        &mut self,
        fence: SettingsFence,
        params: DevelopParams,
    ) -> bool {
        let current = match self.editor.state() {
            EditorState::Ready(doc) => {
                doc.session == fence.session
                    && doc.revision == fence.revision
                    && doc.image == fence.image
                    && !doc.gesture_active()
            }
            _ => false,
        };
        if !current {
            return false;
        }
        let applied = require_finite(&params).and_then(|()| {
            let (key, session) = self.edit_target()?;
            self.run_edit(key, session, EditorCommand::Paste { session, params })
        });
        if let Err(error) = applied {
            Arc::make_mut(&mut self.snapshot).error = Some(error);
        }
        true
    }
}
