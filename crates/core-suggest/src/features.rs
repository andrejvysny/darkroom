//! Feature layout: a MobileCLIP-style image embedding concatenated with hand-computed capture/quality
//! signals. The *order* of [`HandFeatures::to_vec`] is the contract a trained model is fit against —
//! changing it (adding, removing or reordering a field) MUST bump [`FEATURE_VERSION`], because stored
//! weights are position-indexed and would silently mis-read otherwise.
//!
//! Missing values are `f32::NAN`, not 0.0: 0.0 is a *legal* value for most of these signals, so a
//! zero-fill would teach the model that "unknown" looks like "dark / unsharp / no faces". NaNs are
//! replaced at fit time (and at score time) by the weighted training-set column mean.

use crate::error::SuggestError;

/// Bumped whenever the meaning or order of the assembled feature vector changes.
pub const FEATURE_VERSION: u32 = 1;

/// Image-embedding width (MobileCLIP-S1 full-image embedding).
pub const EMB_DIM: usize = 512;

/// Number of hand-computed features appended after the embedding.
pub const HAND_DIM: usize = 16;

/// Total assembled feature width.
pub const DIM: usize = EMB_DIM + HAND_DIM;

/// Hand-computed per-image signals. `f32::NAN` means "unknown" for every field.
///
/// The three `rank_*` fields are the sample's position **within its burst**, in `[0, 1]` (1 = best in
/// burst); a burst of one is `0.5`. The caller computes them because only it knows the burst grouping.
#[derive(Debug, Clone, Copy)]
pub struct HandFeatures {
    pub sharpness_log: f32,
    pub clip_hi: f32,
    pub clip_lo: f32,
    pub dynamic_range_ev: f32,
    pub mean_log_luma: f32,
    pub face_count: f32,
    pub face_max_det: f32,
    pub face_max_quality: f32,
    pub has_face: f32,
    pub log_iso: f32,
    pub log_shutter: f32,
    pub aperture: f32,
    pub focal: f32,
    pub rank_sharpness: f32,
    pub rank_face_quality: f32,
    pub rank_iso: f32,
}

impl Default for HandFeatures {
    /// Everything unknown — every field is imputed at fit/score time.
    fn default() -> Self {
        Self {
            sharpness_log: f32::NAN,
            clip_hi: f32::NAN,
            clip_lo: f32::NAN,
            dynamic_range_ev: f32::NAN,
            mean_log_luma: f32::NAN,
            face_count: f32::NAN,
            face_max_det: f32::NAN,
            face_max_quality: f32::NAN,
            has_face: f32::NAN,
            log_iso: f32::NAN,
            log_shutter: f32::NAN,
            aperture: f32::NAN,
            focal: f32::NAN,
            rank_sharpness: f32::NAN,
            rank_face_quality: f32::NAN,
            rank_iso: f32::NAN,
        }
    }
}

impl HandFeatures {
    /// Fixed feature order — see the module note before touching this.
    pub fn to_vec(&self) -> Vec<f32> {
        vec![
            self.sharpness_log,
            self.clip_hi,
            self.clip_lo,
            self.dynamic_range_ev,
            self.mean_log_luma,
            self.face_count,
            self.face_max_det,
            self.face_max_quality,
            self.has_face,
            self.log_iso,
            self.log_shutter,
            self.aperture,
            self.focal,
            self.rank_sharpness,
            self.rank_face_quality,
            self.rank_iso,
        ]
    }
}

/// Concatenate `emb ‖ hand` into the [`DIM`]-wide training/scoring vector.
pub fn assemble(emb: &[f32], hand: &HandFeatures) -> Result<Vec<f32>, SuggestError> {
    if emb.len() != EMB_DIM {
        return Err(SuggestError::DimMismatch {
            expected: EMB_DIM,
            got: emb.len(),
        });
    }
    let mut x = Vec::with_capacity(DIM);
    x.extend_from_slice(emb);
    x.extend_from_slice(&hand.to_vec());
    Ok(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hand_vec_width_matches_const_and_field_order() {
        let h = HandFeatures {
            sharpness_log: 1.0,
            rank_iso: 16.0,
            ..Default::default()
        };
        let v = h.to_vec();
        assert_eq!(v.len(), HAND_DIM);
        assert_eq!(v[0], 1.0);
        assert_eq!(v[HAND_DIM - 1], 16.0);
        assert!(v[1].is_nan());
    }

    #[test]
    fn assemble_concatenates_and_rejects_bad_embedding() {
        let emb = vec![0.25f32; EMB_DIM];
        let x = assemble(&emb, &HandFeatures::default()).unwrap();
        assert_eq!(x.len(), DIM);
        assert_eq!(x[EMB_DIM - 1], 0.25);
        assert!(x[EMB_DIM].is_nan());
        assert_eq!(
            assemble(&[0.0; 8], &HandFeatures::default()),
            Err(SuggestError::DimMismatch {
                expected: EMB_DIM,
                got: 8
            })
        );
    }
}
