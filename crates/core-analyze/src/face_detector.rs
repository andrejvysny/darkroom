//! Face detector — SCRFD-10G-KPS (InsightFace `buffalo_l/det_10g`) via ONNX Runtime + CoreML.
//!
//! Recipe (InsightFace `scrfd.py`): input `[1,3,S,S]` (top-left black-pad letterbox, `(px−127.5)/128`,
//! RGB) → 9 outputs = score/bbox/kps for strides 8/16/32, 2 anchors per cell. Boxes are distance-decoded
//! relative to anchor centers (`cx=col·s, cy=row·s`); landmarks are 5 offset pairs. We map both back to
//! source pixels (÷scale), keep boxes normalized `[0,1]`, and run greedy IoU NMS.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Mutex;

use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::error::AnalyzeError;
use crate::{models, preprocess};

const STRIDES: [u32; 3] = [8, 16, 32];
const NUM_ANCHORS: usize = 2;
const DEFAULT_DET_THRESH: f32 = 0.5;
const DEFAULT_NMS_IOU: f32 = 0.4;

/// One detected face: box normalized `[x0,y0,x1,y1]` in `[0,1]`, 5 landmarks in **source pixels**
/// (for alignment), and the detector score.
#[derive(Debug, Clone)]
pub struct FaceDetection {
    pub bbox: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub score: f32,
}

/// A face detector producing boxes + 5-point landmarks. Trait so the embedder/detector pair can be
/// swapped (e.g. to a commercial-licensed YuNet) without touching the pass or storage.
pub trait FaceDetector: Send + Sync {
    fn detect(&self, img: &RgbImage) -> Result<Vec<FaceDetection>, AnalyzeError>;
}

pub struct Scrfd {
    session: Mutex<Session>,
    input_name: String,
    input_size: u32,
    det_thresh: f32,
    nms_iou: f32,
}

impl Scrfd {
    /// Load the SCRFD ONNX. `input_size` (multiple of 32; 640 default) is the fixed square fed to the
    /// CoreML MLProgram EP — raise (e.g. 1024) for better small-face recall at a speed cost.
    pub fn new(model_path: &Path, input_size: u32) -> Result<Self, AnalyzeError> {
        // Fixed square input → MLProgram + static_input_shapes (same recipe as D-FINE/MegaDetector),
        // keeping FP32-ish intermediates so scores stay stable for thresholding.
        let session = models::build_session(model_path, false, true)?;
        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "input.1".into());
        Ok(Self {
            session: Mutex::new(session),
            input_name,
            input_size,
            det_thresh: env_f32("DARKROOM_FACE_DET_THRESH", DEFAULT_DET_THRESH),
            nms_iou: env_f32("DARKROOM_FACE_NMS_IOU", DEFAULT_NMS_IOU),
        })
    }
}

impl FaceDetector for Scrfd {
    fn detect(&self, img: &RgbImage) -> Result<Vec<FaceDetection>, AnalyzeError> {
        let s = self.input_size;
        let (chw, scale) = preprocess::to_scrfd_chw(img, s);
        let input = Tensor::from_array(([1usize, 3, s as usize, s as usize], chw))
            .map_err(AnalyzeError::inference)?;

        // Extract all 9 outputs as owned (n_rows, data), bucketed by trailing dim (1=score,4=bbox,10=kps).
        let (mut scores, mut bboxes, mut kpss): (Vec<(usize, Vec<f32>)>, _, _) = (
            Vec::new(),
            Vec::<(usize, Vec<f32>)>::new(),
            Vec::<(usize, Vec<f32>)>::new(),
        );
        {
            let mut session = self.session.lock().expect("scrfd mutex poisoned");
            let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> =
                vec![(Cow::Owned(self.input_name.clone()), input.into())];
            let outputs = session.run(inputs).map_err(AnalyzeError::inference)?;
            for i in 0..outputs.len() {
                let arr = outputs[i]
                    .try_extract_array::<f32>()
                    .map_err(AnalyzeError::inference)?;
                let last = arr.shape().last().copied().unwrap_or(0);
                let total = arr.len();
                if last == 0 {
                    continue;
                }
                let n = total / last;
                let data: Vec<f32> = arr.iter().copied().collect();
                match last {
                    1 => scores.push((n, data)),
                    4 => bboxes.push((n, data)),
                    10 => kpss.push((n, data)),
                    _ => {}
                }
            }
        }
        if scores.len() < 3 || bboxes.len() < 3 || kpss.len() < 3 {
            return Err(AnalyzeError::Inference(format!(
                "SCRFD expected 3×(score,bbox,kps); got {}/{}/{}",
                scores.len(),
                bboxes.len(),
                kpss.len()
            )));
        }
        // Strides correspond to descending row counts (8→largest feature map).
        scores.sort_by(|a, b| b.0.cmp(&a.0));
        bboxes.sort_by(|a, b| b.0.cmp(&a.0));
        kpss.sort_by(|a, b| b.0.cmp(&a.0));

        let (ow, oh) = (img.width() as f32, img.height() as f32);
        let inv = 1.0 / scale;
        let mut faces: Vec<FaceDetection> = Vec::new();
        for k in 0..3 {
            let stride = STRIDES[k] as f32;
            let width = (s / STRIDES[k]) as usize;
            let (n, sc) = (&scores[k].0, &scores[k].1);
            let bb = &bboxes[k].1;
            let kp = &kpss[k].1;
            for idx in 0..*n {
                let score = sc[idx];
                if score < self.det_thresh {
                    continue;
                }
                let cell = idx / NUM_ANCHORS;
                let cx = (cell % width) as f32 * stride;
                let cy = (cell / width) as f32 * stride;
                // Distance-decode box (offsets are in stride units) → input px → source px → normalized.
                let (d0, d1, d2, d3) = (
                    bb[idx * 4] * stride,
                    bb[idx * 4 + 1] * stride,
                    bb[idx * 4 + 2] * stride,
                    bb[idx * 4 + 3] * stride,
                );
                let bbox = [
                    ((cx - d0) * inv / ow).clamp(0.0, 1.0),
                    ((cy - d1) * inv / oh).clamp(0.0, 1.0),
                    ((cx + d2) * inv / ow).clamp(0.0, 1.0),
                    ((cy + d3) * inv / oh).clamp(0.0, 1.0),
                ];
                if bbox[2] <= bbox[0] || bbox[3] <= bbox[1] {
                    continue;
                }
                let mut kps = [[0f32; 2]; 5];
                for (j, kp_pt) in kps.iter_mut().enumerate() {
                    kp_pt[0] = (cx + kp[idx * 10 + 2 * j] * stride) * inv;
                    kp_pt[1] = (cy + kp[idx * 10 + 2 * j + 1] * stride) * inv;
                }
                faces.push(FaceDetection { bbox, kps, score });
            }
        }
        Ok(nms(faces, self.nms_iou))
    }
}

fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = a[2].min(b[2]);
    let y1 = a[3].min(b[3]);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Greedy non-max suppression (single class) — highest score wins, carries landmarks through.
fn nms(mut faces: Vec<FaceDetection>, iou_thresh: f32) -> Vec<FaceDetection> {
    faces.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<FaceDetection> = Vec::new();
    'outer: for f in faces {
        for k in &keep {
            if iou(&k.bbox, &f.bbox) > iou_thresh {
                continue 'outer;
            }
        }
        keep.push(f);
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nms_suppresses_overlap() {
        let mk = |bbox: [f32; 4], score: f32| FaceDetection {
            bbox,
            kps: [[0.0; 2]; 5],
            score,
        };
        let a = mk([0.10, 0.10, 0.30, 0.30], 0.9);
        let b = mk([0.11, 0.11, 0.31, 0.31], 0.8); // heavy overlap with a → dropped
        let c = mk([0.60, 0.60, 0.80, 0.80], 0.7); // disjoint → kept
        let kept = nms(vec![a, b, c], 0.4);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
    }
}
