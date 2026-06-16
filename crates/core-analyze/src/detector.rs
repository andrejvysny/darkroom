//! Object detection analyzer — D-FINE (Apache-2.0) via ONNX Runtime + CoreML.
//!
//! Recipe validated in Phase 0 (see SPIKE.md): input `pixel_values[1,3,640,640]` (resize-squash,
//! ÷255, no mean/std) → `logits[1,300,80]` + `pred_boxes[1,300,4]` (cxcywh, normalized). DETR-style,
//! NMS-free — we apply only a light per-label IoU dedup for occasional near-duplicate queries.

use std::path::Path;
use std::sync::Mutex;

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::AnalyzeError;
use crate::{
    coco, models, preprocess, AnalysisCtx, AnalysisRecord, Analyzer, Detection, DetectionPayload,
};

const INPUT: u32 = 640;
const DEFAULT_THRESHOLD: f32 = 0.45;
const DEDUP_IOU: f32 = 0.6;

pub struct ObjectDetector {
    session: Mutex<Session>,
    model_version: &'static str,
    threshold: f32,
}

impl ObjectDetector {
    /// Load a D-FINE ONNX model. `model_version` is a stable tag stored per result row (bump to force
    /// re-analysis). The detector loads at the default optimization level.
    pub fn new(model_path: &Path, model_version: &'static str) -> Result<Self, AnalyzeError> {
        let session = models::build_session(model_path, false)?;
        Ok(Self {
            session: Mutex::new(session),
            model_version,
            threshold: DEFAULT_THRESHOLD,
        })
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    /// Detect target-class (People/Animals/Vehicles) objects in an sRGB image. Boxes are returned
    /// normalized to `[0,1]` (decode-size-independent), so the caller may feed a downscaled image.
    pub fn detect(&self, img: &RgbImage) -> Result<Vec<Detection>, AnalyzeError> {
        let chw = preprocess::to_chw(img, INPUT, [0.0; 3], [1.0; 3]);
        let input = Tensor::from_array(([1usize, 3, INPUT as usize, INPUT as usize], chw))
            .map_err(AnalyzeError::inference)?;

        let mut session = self.session.lock().expect("detector mutex poisoned");
        let outputs = session
            .run(ort::inputs![input])
            .map_err(AnalyzeError::inference)?;

        // Locate logits ([.,.,80]) + boxes ([.,.,4]) by trailing dim — order-independent.
        let mut logits = None;
        let mut boxes = None;
        for i in 0..outputs.len() {
            let arr = outputs[i]
                .try_extract_array::<f32>()
                .map_err(AnalyzeError::inference)?;
            match arr.shape().last().copied() {
                Some(80) => logits = Some(arr),
                Some(4) => boxes = Some(arr),
                _ => {}
            }
        }
        let logits = logits.ok_or_else(|| AnalyzeError::Inference("no [.,.,80] logits".into()))?;
        let boxes = boxes.ok_or_else(|| AnalyzeError::Inference("no [.,.,4] boxes".into()))?;
        let nq = logits.shape()[1];

        let mut dets = Vec::new();
        for q in 0..nq {
            let (mut best_c, mut best_s) = (0usize, 0f32);
            for c in 0..80 {
                let s = sigmoid(logits[[0, q, c]]);
                if s > best_s {
                    best_s = s;
                    best_c = c;
                }
            }
            if best_s < self.threshold {
                continue;
            }
            let label = coco::COCO_LABELS[best_c];
            let Some(cat) = coco::category(label) else {
                continue;
            }; // drop non-target classes
            let (cx, cy, w, h) = (
                boxes[[0, q, 0]],
                boxes[[0, q, 1]],
                boxes[[0, q, 2]],
                boxes[[0, q, 3]],
            );
            dets.push(Detection {
                label: label.to_string(),
                category: cat.to_string(),
                confidence: best_s,
                // Normalized [0,1] (decode-size-independent).
                bbox: [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0],
            });
        }
        Ok(dedup(dets))
    }
}

impl Analyzer for ObjectDetector {
    fn id(&self) -> &'static str {
        crate::OBJECT_DETECTION_ID
    }

    fn model_version(&self) -> &'static str {
        self.model_version
    }

    fn analyze(&self, ctx: &AnalysisCtx) -> Result<AnalysisRecord, AnalyzeError> {
        let detections = self.detect(ctx.image)?;
        let payload = serde_json::to_value(DetectionPayload { detections })
            .map_err(|e| AnalyzeError::Other(e.to_string()))?;
        Ok(AnalysisRecord::new(self.id(), self.model_version, payload))
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
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

/// Greedy per-label IoU suppression of near-duplicate boxes (highest confidence wins).
fn dedup(mut dets: Vec<Detection>) -> Vec<Detection> {
    dets.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<Detection> = Vec::new();
    'outer: for d in dets {
        for k in &keep {
            if k.label == d.label && iou(&k.bbox, &d.bbox) > DEDUP_IOU {
                continue 'outer;
            }
        }
        keep.push(d);
    }
    keep
}
