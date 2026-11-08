//! The cached picture shown while the live render is owed. A placeholder is used only when it is
//! GEOMETRY-EXACT for the selection and stage variant — never a stand-in: an edited photo is
//! answered only by its edited tiers, the Before view only by unedited ones, the crop tool by
//! nothing. Tier order is resident thumbnail first (instant), then the large disk preview.

use crate::stage_preview::{PreviewQuery, StagePreview};
use crate::thumbs::{ThumbKey, ThumbStore};
use core_app::state::ApplicationSnapshot;
use core_model::catalog::ImageRow;
use core_model::identity::ImageId;
use ui_egui::{target_id, Picture, PresentId, StageVariant, SAVED_GEOMETRY};

/// Where to look for a row under a variant.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Tiers {
    /// The exact resident thumbnail key.
    pub resident: ThumbKey,
    /// The disk preview tier: `Some(v)` = the edited tier of version `v`, `None` = unedited.
    pub disk: Option<i64>,
}

/// The tiers that are exact for `row` under `variant`, or `None` when no cached picture can be.
pub(super) fn plan_tiers(variant: StageVariant, row: &ImageRow) -> Option<Tiers> {
    let hash = row.content_hash.to_ascii_lowercase();
    let edited = match variant {
        StageVariant::Edited => row.edited_at,
        // The unedited photo, whatever the saved edit is.
        StageVariant::Before => None,
        // The whole frame is rendered live; nothing cached has that framing.
        StageVariant::CropTool => return None,
    };
    Some(Tiers {
        resident: ThumbKey {
            image_id: row.id,
            hash,
            edited_at: edited,
        },
        disk: edited,
    })
}

/// The selected row: the editor's once loaded, else the library list's (so the picture can show
/// before the editor's own row query answers).
fn selected_row(snapshot: &ApplicationSnapshot) -> Option<&ImageRow> {
    let selected = snapshot.editor.selected?;
    snapshot
        .editor
        .image
        .as_ref()
        .filter(|row| ImageId(row.id) == selected)
        .or_else(|| {
            snapshot
                .library
                .rows
                .iter()
                .find(|row| ImageId(row.id) == selected)
        })
}

/// The framing of the SAVED edit of `row`, known once its document is `Ready` and still unedited
/// (not dirty): exactly the geometry a placeholder rendered from the saved edit has.
fn saved_target(
    snapshot: &ApplicationSnapshot,
    row: &ImageRow,
    variant: StageVariant,
) -> Option<PresentId> {
    snapshot
        .editor
        .image
        .as_ref()
        .filter(|editor_row| editor_row.id == row.id && !snapshot.editor.document.dirty)
        .and_then(|_| target_id(snapshot, variant))
        .filter(|target| target.geometry != SAVED_GEOMETRY)
}

/// The id a placeholder of `row` carries: the saved edit's framing when the document is known,
/// else the saved-geometry sentinel. The saved edit IS what the document loads, so the sentinel
/// is pinned to the real signature from the document ([`pin_geometry`]) — never from a live
/// frame, which may already show an unsaved crop.
fn present_id(snapshot: &ApplicationSnapshot, row: &ImageRow, variant: StageVariant) -> PresentId {
    saved_target(snapshot, row, variant).unwrap_or(PresentId {
        image: ImageId(row.id),
        edit_version: row.edited_at,
        variant,
        geometry: SAVED_GEOMETRY,
    })
}

/// Pin a sentinel placeholder to the saved edit's framing (same image, edit version and variant):
/// from then on a geometry edit makes it incompatible (not painted) as it must.
pub(super) fn pin_geometry(held: &mut PresentId, live: &PresentId) {
    if held.geometry == SAVED_GEOMETRY
        && held.image == live.image
        && held.edit_version == live.edit_version
        && held.variant == live.variant
    {
        held.geometry = live.geometry;
    }
}

struct Held {
    // Owning handle: a bare TextureId dies when the thumbnail store evicts its resident.
    handle: egui::TextureHandle,
    px: [u32; 2],
    id: PresentId,
}

#[derive(Debug, PartialEq, Eq)]
struct Key {
    image_id: i64,
    hash: String,
    edited_at: Option<i64>,
    variant: StageVariant,
}

#[derive(Default)]
pub(super) struct Placeholders {
    loader: Option<StagePreview>,
    key: Option<Key>,
    held: Option<Held>,
    /// Echoed by the loader so an answer for a photo we moved on from is dropped.
    token: u64,
    requested: bool,
    pub variant: StageVariant,
}

impl Placeholders {
    pub fn attach(&mut self, loader: StagePreview) {
        self.loader = Some(loader);
    }

    /// Device loss: no texture of the old device survives. The token moves on so a result still
    /// in flight for the old epoch is dropped, and queued results are discarded unread.
    pub fn retire(&mut self) {
        self.clear();
        self.requested = false;
        self.token += 1;
        if let Some(loader) = &self.loader {
            drop(loader.drain());
        }
    }

    fn clear(&mut self) {
        self.key = None;
        self.held = None;
    }

    /// Refresh against this frame's snapshot. Returns the image when a placeholder was installed
    /// or upgraded this frame (the "a cached picture is up" milestone).
    pub fn update(
        &mut self,
        snapshot: &ApplicationSnapshot,
        ctx: &egui::Context,
        thumbs: Option<&mut ThumbStore>,
        edge: &dyn Fn() -> u32,
    ) -> Option<ImageId> {
        let tiers = snapshot
            .catalog_ready
            .then(|| selected_row(snapshot))
            .flatten()
            .and_then(|row| plan_tiers(self.variant, row).map(|tiers| (row, tiers)));
        let Some((row, tiers)) = tiers else {
            self.clear();
            return None;
        };
        let key = Key {
            image_id: row.id,
            hash: tiers.resident.hash.clone(),
            edited_at: tiers.disk,
            variant: self.variant,
        };
        if self.key.as_ref() != Some(&key) {
            self.held = None;
            self.key = Some(key);
            self.requested = false;
            self.token += 1;
        }
        let id = self
            .held
            .as_ref()
            .map(|held| held.id)
            .unwrap_or_else(|| present_id(snapshot, row, self.variant));
        if let (Some(held), Some(saved)) = (
            self.held.as_mut(),
            saved_target(snapshot, row, self.variant),
        ) {
            pin_geometry(&mut held.id, &saved);
        }
        let mut installed = false;
        for result in self.loader.iter().flat_map(StagePreview::drain) {
            if result.token != self.token {
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [result.size[0] as usize, result.size[1] as usize],
                &result.rgba,
            );
            let handle = ctx.load_texture(
                format!("stage-preview-{}", row.id),
                image,
                egui::TextureOptions::LINEAR,
            );
            self.held = Some(Held {
                handle,
                px: result.size,
                id,
            });
            installed = true;
        }
        if self.held.is_none() {
            let resident = thumbs.and_then(|store| store.exact_resident(&tiers.resident));
            if let Some((handle, px)) = resident {
                self.held = Some(Held { handle, px, id });
                installed = true;
            }
        }
        if !self.requested {
            self.requested = true;
            if let Some(loader) = &self.loader {
                loader.request(PreviewQuery {
                    token: self.token,
                    hash: tiers.resident.hash.clone(),
                    edited_at: tiers.disk,
                    edge: edge(),
                });
            }
        }
        installed.then_some(ImageId(row.id))
    }

    /// The placeholder, only for `image` (the current selection).
    pub fn picture(&self, image: Option<ImageId>) -> Option<Picture> {
        let held = self.held.as_ref()?;
        (Some(held.id.image) == image).then(|| Picture {
            texture: held.handle.id(),
            px: held.px,
            id: held.id,
            window: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(edited_at: Option<i64>) -> ImageRow {
        ImageRow {
            id: 4,
            content_hash: "AABB".into(),
            edited_at,
            ..Default::default()
        }
    }

    fn key(edited_at: Option<i64>) -> ThumbKey {
        ThumbKey {
            image_id: 4,
            hash: "aabb".into(),
            edited_at,
        }
    }

    #[test]
    fn retire_drops_the_held_texture_and_invalidates_the_token() {
        let ctx = egui::Context::default();
        let handle = ctx.load_texture("t", egui::ColorImage::example(), Default::default());
        let id = PresentId {
            image: ImageId(4),
            edit_version: None,
            variant: StageVariant::Edited,
            geometry: SAVED_GEOMETRY,
        };
        let mut placeholders = Placeholders {
            held: Some(Held {
                handle,
                px: [1, 1],
                id,
            }),
            token: 3,
            requested: true,
            ..Default::default()
        };
        assert!(placeholders.picture(Some(ImageId(4))).is_some());
        placeholders.retire();
        assert!(placeholders.picture(Some(ImageId(4))).is_none());
        assert_eq!(placeholders.token, 4);
        assert!(!placeholders.requested);
    }

    #[test]
    fn an_edited_row_is_answered_only_by_its_exact_edit_version() {
        let tiers = plan_tiers(StageVariant::Edited, &row(Some(9))).unwrap();
        assert_eq!(tiers.resident, key(Some(9)));
        assert_eq!(tiers.disk, Some(9));
    }

    #[test]
    fn an_unedited_row_uses_the_plain_tiers() {
        let tiers = plan_tiers(StageVariant::Edited, &row(None)).unwrap();
        assert_eq!((tiers.resident, tiers.disk), (key(None), None));
    }

    #[test]
    fn before_uses_unedited_tiers_even_for_an_edited_row() {
        let tiers = plan_tiers(StageVariant::Before, &row(Some(9))).unwrap();
        assert_eq!((tiers.resident, tiers.disk), (key(None), None));
    }

    #[test]
    fn the_crop_tool_has_no_placeholder() {
        assert!(plan_tiers(StageVariant::CropTool, &row(Some(9))).is_none());
        assert!(plan_tiers(StageVariant::CropTool, &row(None)).is_none());
    }

    fn id(geometry: u64, version: Option<i64>) -> PresentId {
        PresentId {
            image: ImageId(4),
            edit_version: version,
            variant: StageVariant::Edited,
            geometry,
        }
    }

    #[test]
    fn the_sentinel_pins_to_the_first_matching_live_frame_only() {
        let mut held = id(SAVED_GEOMETRY, Some(9));
        pin_geometry(&mut held, &id(5, Some(8)));
        assert_eq!(held.geometry, SAVED_GEOMETRY, "other edit version");
        pin_geometry(&mut held, &id(5, Some(9)));
        assert_eq!(held.geometry, 5);
        pin_geometry(&mut held, &id(6, Some(9)));
        assert_eq!(
            held.geometry, 5,
            "already pinned: a later geometry edit is not absorbed"
        );
    }
}
