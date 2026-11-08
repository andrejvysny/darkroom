//! Presentation identity of the pictures the stage paints: what a picture is OF
//! ([`PresentId`]), the texture + window that shows it ([`Picture`]), and the framing signature.

use super::StageVariant;
use core_model::identity::ImageId;
use core_model::DevelopParams;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};

/// Geometry value of a picture that shows "the saved edit's geometry" without having measured it
/// (a cached thumbnail/preview of the persisted edit, painted before the document is `Ready`).
/// The saved edit IS what the document loads, so such a picture is compatible with every target
/// of the same image + edit version; the host pins it to the first live frame's real signature.
pub const SAVED_GEOMETRY: u64 = u64::MAX;

/// Hash of everything that changes WHERE pixels land: crop (`cx cy hw hh angle rot90`), border
/// (`size color aspect_w aspect_h`) and lens geometry (`dist_k1 dist_k2 ca_red ca_blue`), by `f32`
/// bit pattern. Tone/colour/detail fields are deliberately absent: a picture with other tones is
/// still the right framing, so it may be shown (reprojected) while the new render is owed.
pub fn geometry_sig(params: &DevelopParams) -> u64 {
    let mut hasher = DefaultHasher::new();
    let crop = &params.crop;
    for v in [crop.cx, crop.cy, crop.hw, crop.hh, crop.angle] {
        v.to_bits().hash(&mut hasher);
    }
    crop.rot90.hash(&mut hasher);
    let border = &params.border;
    for v in border
        .color
        .into_iter()
        .chain([border.size, border.aspect_w, border.aspect_h])
    {
        v.to_bits().hash(&mut hasher);
    }
    for v in [
        params.dist_k1,
        params.dist_k2,
        params.ca_red,
        params.ca_blue,
    ] {
        v.to_bits().hash(&mut hasher);
    }
    // Never collide with the sentinel.
    hasher.finish().min(SAVED_GEOMETRY - 1)
}

/// What a picture is a picture OF: image, the edit version of the catalog row it shows (`None` =
/// unedited), which stage variant, and its framing signature ([`geometry_sig`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentId {
    pub image: ImageId,
    pub edit_version: Option<i64>,
    pub variant: StageVariant,
    pub geometry: u64,
}

impl PresentId {
    /// May `self` be painted on a stage whose target is `target`? Same image, edit version and
    /// variant, and the same framing (a [`SAVED_GEOMETRY`] side matches any).
    pub fn compatible(&self, target: &PresentId) -> bool {
        self.image == target.image
            && self.edit_version == target.edit_version
            && self.variant == target.variant
            && (self.geometry == target.geometry
                || self.geometry == SAVED_GEOMETRY
                || target.geometry == SAVED_GEOMETRY)
    }
}

/// A registered texture the stage can paint, with what it shows. Live frames come from the
/// photo worker; placeholders from cached thumbnails/previews.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picture {
    pub texture: egui::TextureId,
    pub px: [u32; 2],
    pub id: PresentId,
    /// The crop-local `(origin, size)` UV window the pixels show; `None` = the whole frame.
    pub window: Option<([f32; 2], [f32; 2])>,
}

impl Picture {
    /// A whole-frame picture of no particular edit (tests and callers that never compare ids:
    /// live frames are not subject to the compatibility rule, only placeholders are).
    pub fn bare(texture: egui::TextureId, px: [u32; 2]) -> Self {
        Self {
            texture,
            px,
            id: PresentId {
                image: ImageId(0),
                edit_version: None,
                variant: StageVariant::Edited,
                geometry: SAVED_GEOMETRY,
            },
            window: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(geometry: u64) -> PresentId {
        PresentId {
            image: ImageId(1),
            edit_version: Some(5),
            variant: StageVariant::Edited,
            geometry,
        }
    }

    #[test]
    fn geometry_sig_follows_only_geometry_fields() {
        let base = DevelopParams::default();
        let sig = geometry_sig(&base);
        let tonal = DevelopParams {
            exposure: 1.5,
            contrast: 20.0,
            vignette: 30.0,
            clarity: 10.0,
            ..Default::default()
        };
        assert_eq!(
            geometry_sig(&tonal),
            sig,
            "tone/colour/detail are not framing"
        );
        let mut cropped = DevelopParams::default();
        cropped.crop.hw = 0.3;
        let mut rotated = DevelopParams::default();
        rotated.crop.rot90 = 1;
        let mut straight = DevelopParams::default();
        straight.crop.angle = 2.0;
        let mut bordered = DevelopParams::default();
        bordered.border.size = 4.0;
        let lens = DevelopParams {
            dist_k1: 10.0,
            ..Default::default()
        };
        let ca = DevelopParams {
            ca_blue: -5.0,
            ..Default::default()
        };
        let sigs = [cropped, rotated, straight, bordered, lens, ca].map(|p| geometry_sig(&p));
        for (i, s) in sigs.iter().enumerate() {
            assert_ne!(*s, sig, "geometry field #{i} must change the signature");
            assert_ne!(*s, SAVED_GEOMETRY);
        }
    }

    #[test]
    fn compatibility_needs_image_version_variant_and_framing() {
        let target = id(7);
        assert!(id(7).compatible(&target));
        assert!(!id(8).compatible(&target), "other framing");
        assert!(
            id(SAVED_GEOMETRY).compatible(&target),
            "saved framing matches the loaded edit"
        );
        assert!(
            id(8).compatible(&id(SAVED_GEOMETRY)),
            "target not loaded yet"
        );
        let mut other = id(7);
        other.edit_version = Some(6);
        assert!(!other.compatible(&target));
        other = id(7);
        other.variant = StageVariant::Before;
        assert!(!other.compatible(&target));
        other = id(7);
        other.image = ImageId(2);
        assert!(!other.compatible(&target));
    }
}
