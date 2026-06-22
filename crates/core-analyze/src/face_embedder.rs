//! Face embedder — ArcFace `w600k_r50` (InsightFace `buffalo_l/recognition`) via ONNX Runtime + CoreML.
//!
//! Input: aligned 112×112 RGB face crop, normalized `(px−127.5)/127.5` (i.e. `to_chw` with mean 0.5,
//! std 0.5). Output: a 512-d embedding, L2-normalized here so cosine similarity == dot product and
//! cosine/Euclidean distances are rank-equivalent for clustering.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Mutex;

use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::error::AnalyzeError;
use crate::{models, preprocess};

const EMBED_INPUT: u32 = 112;
const EMBED_DIM: usize = 512;

/// A face embedder mapping an aligned 112×112 crop to an L2-normalized vector. Trait so the model can
/// be swapped (e.g. to a commercial-licensed SFace) without touching the pass, clustering, or storage.
pub trait FaceEmbedder: Send + Sync {
    fn embed(&self, aligned: &RgbImage) -> Result<Vec<f32>, AnalyzeError>;
    fn dim(&self) -> usize;
}

pub struct ArcFace {
    session: Mutex<Session>,
    input_name: String,
}

impl ArcFace {
    pub fn new(model_path: &Path) -> Result<Self, AnalyzeError> {
        let session = models::build_session(model_path, false, true)?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "input.1".into());
        Ok(Self {
            session: Mutex::new(session),
            input_name,
        })
    }
}

impl FaceEmbedder for ArcFace {
    fn embed(&self, aligned: &RgbImage) -> Result<Vec<f32>, AnalyzeError> {
        // (px/255 − 0.5)/0.5 == (px − 127.5)/127.5, RGB, planar CHW.
        let chw = preprocess::to_chw(aligned, EMBED_INPUT, [0.5; 3], [0.5; 3]);
        let input =
            Tensor::from_array(([1usize, 3, EMBED_INPUT as usize, EMBED_INPUT as usize], chw))
                .map_err(AnalyzeError::inference)?;
        let mut session = self.session.lock().expect("arcface mutex poisoned");
        let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> =
            vec![(Cow::Owned(self.input_name.clone()), input.into())];
        let outputs = session.run(inputs).map_err(AnalyzeError::inference)?;
        let arr = outputs[0]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;
        let mut v: Vec<f32> = arr.iter().copied().collect();
        l2_normalize(&mut v);
        Ok(v)
    }

    fn dim(&self) -> usize {
        EMBED_DIM
    }
}

/// In-place L2 normalization (no-op for a zero vector).
pub fn l2_normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_unit_norm() {
        let mut v = vec![3.0f32, 4.0];
        l2_normalize(&mut v);
        let n = (v[0] * v[0] + v[1] * v[1]).sqrt();
        assert!((n - 1.0).abs() < 1e-6);
        assert!((v[0] - 0.6).abs() < 1e-6 && (v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_zero_is_noop() {
        let mut v = vec![0.0f32; 4];
        l2_normalize(&mut v);
        assert!(v.iter().all(|&x| x == 0.0));
    }
}
