//! Object detection analyzer — D-FINE (Apache-2.0) via ONNX Runtime + CoreML.
//!
//! Recipe validated in Phase 0 (see SPIKE.md): input `pixel_values[1,3,640,640]` (resize-squash,
//! ÷255, no mean/std) → `logits[1,300,80]` + `pred_boxes[1,300,4]` (cxcywh, normalized). DETR-style,
//! NMS-free — we apply only a light per-label IoU dedup for occasional near-duplicate queries.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use crate::error::AnalyzeError;
use crate::{
    coco, models, preprocess, AnalysisCtx, AnalysisRecord, Analyzer, Detection, DetectionPayload,
    RawScore, Verifier,
};

const INPUT: u32 = 640;
/// Absolute candidate floor. D-FINE's focal-loss sigmoid heads have NO background class, so every one
/// of the 300 queries always emits a best-guess; on featureless frames unmatched queries score in the
/// ~0.45–0.60 noise band (`sigmoid(−0.2)≈0.45`). Nothing below this floor is ever considered; the
/// per-category gate in `coco::threshold` then sets the real accept bar. Lowered 0.50→0.40 (v3) so the
/// CLIP verifier — not this floor — supplies People precision (recovers distant/back-turned people the
/// 0.55 gate dropped, while the strict person verifier-accept rejects the texture false positives).
const DEFAULT_THRESHOLD: f32 = 0.40;
/// Reject ambiguous "background" queries whose class distribution is flat: a real detection's top
/// class dominates the runner-up, whereas a noise query has `best ≈ second`. Keep only if
/// `best_s ≥ MARGIN_RATIO × second_s`.
const MARGIN_RATIO: f32 = 1.5;
const DEDUP_IOU: f32 = 0.6;

// Box-sanity bounds (normalized [0,1] coords). Kill degenerate noise boxes that survive thresholding.
const MIN_AREA: f32 = 0.003; // dust-speck boxes
const MAX_AREA: f32 = 0.85; // near-whole-frame "detections"
const PERSON_MAX_ASPECT: f32 = 1.5; // people are taller than wide; very wide person boxes are noise
const EDGE_EPS: f32 = 0.01;
const TINY_EDGE_AREA: f32 = 0.01; // small boxes hugging a frame edge are usually artifacts

pub struct ObjectDetector {
    session: Mutex<Session>,
    model_version: &'static str,
    threshold: f32,
    margin: f32,
    verifier: Option<Arc<Verifier>>,
}

impl ObjectDetector {
    /// Load a D-FINE ONNX model. `model_version` is a stable tag stored per result row (bump to force
    /// re-analysis). The detector loads at the default optimization level. The confidence floor and
    /// margin take their defaults from [`DEFAULT_THRESHOLD`]/[`MARGIN_RATIO`], overridable for offline
    /// calibration only via `DARKROOM_DET_FLOOR`/`DARKROOM_DET_MARGIN` (production must NOT set these).
    pub fn new(model_path: &Path, model_version: &'static str) -> Result<Self, AnalyzeError> {
        let session = models::build_session(model_path, false, true)?;
        Ok(Self {
            session: Mutex::new(session),
            model_version,
            threshold: env_f32("DARKROOM_DET_FLOOR", DEFAULT_THRESHOLD),
            margin: env_f32("DARKROOM_DET_MARGIN", MARGIN_RATIO),
            verifier: None,
        })
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = threshold;
        self
    }

    /// Attach a CLIP verifier — every kept detection is confirmed by a crop re-check (kills
    /// confident-but-wrong boxes like a poppy scored `person`).
    pub fn with_verifier(mut self, verifier: Arc<Verifier>) -> Self {
        self.verifier = Some(verifier);
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
            // Top-2 sigmoid scores across the 80 classes (for the argmax + the margin gate).
            let (mut best_c, mut best_s, mut second_s) = (0usize, 0f32, 0f32);
            for c in 0..80 {
                let s = sigmoid(logits[[0, q, c]]);
                if s > best_s {
                    second_s = best_s;
                    best_s = s;
                    best_c = c;
                } else if s > second_s {
                    second_s = s;
                }
            }
            // Absolute floor + flat-distribution (background) rejection.
            if best_s < self.threshold || best_s < self.margin * second_s {
                continue;
            }
            let label = coco::COCO_LABELS[best_c];
            let Some(cat) = coco::category(label) else {
                continue; // drop non-target classes (Animals owned by MegaDetector)
            };
            // Per-category accept gate.
            if best_s < coco::threshold(cat) {
                continue;
            }
            let (cx, cy, w, h) = (
                boxes[[0, q, 0]],
                boxes[[0, q, 1]],
                boxes[[0, q, 2]],
                boxes[[0, q, 3]],
            );
            // Normalized [0,1] xyxy (decode-size-independent).
            let bbox = [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0];
            if !box_ok(label, &bbox) {
                continue;
            }
            // CLIP crop re-check (if attached) — drops confident-but-wrong detections.
            if let Some(v) = &self.verifier {
                if !v.confirm(img, &bbox, cat)? {
                    continue;
                }
            }
            dets.push(Detection {
                label: label.to_string(),
                category: cat.to_string(),
                confidence: best_s,
                bbox,
            });
        }
        Ok(dedup(dets))
    }

    /// Diagnostic per-category **pre-gate** scores for offline threshold sweeping (NOT production).
    /// Mirrors [`detect`](Self::detect)'s query loop — same absolute floor + `MARGIN_RATIO` + box
    /// sanity that define a real candidate — but skips the per-category [`coco::threshold`] accept
    /// gate, tracking `best_raw = max(best_raw, best_s)` per category. For each category's top
    /// candidate it then captures the verifier's positive-prompt prob and the decision production
    /// `detect()` would make (`gated = best_raw ≥ coco::threshold && verifier accepts`).
    pub fn score_raw(
        &self,
        img: &RgbImage,
    ) -> Result<HashMap<&'static str, RawScore>, AnalyzeError> {
        let chw = preprocess::to_chw(img, INPUT, [0.0; 3], [1.0; 3]);
        let input = Tensor::from_array(([1usize, 3, INPUT as usize, INPUT as usize], chw))
            .map_err(AnalyzeError::inference)?;

        // Best (score, bbox) per D-FINE-owned category, collected under the session lock (the tensor
        // views borrow `outputs` which borrows `session`, so scalars must be captured before unlock).
        let mut top: HashMap<&'static str, (f32, [f32; 4])> = HashMap::new();
        {
            let mut session = self.session.lock().expect("detector mutex poisoned");
            let outputs = session
                .run(ort::inputs![input])
                .map_err(AnalyzeError::inference)?;
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
            let logits =
                logits.ok_or_else(|| AnalyzeError::Inference("no [.,.,80] logits".into()))?;
            let boxes = boxes.ok_or_else(|| AnalyzeError::Inference("no [.,.,4] boxes".into()))?;
            let nq = logits.shape()[1];
            for q in 0..nq {
                let (mut best_c, mut best_s, mut second_s) = (0usize, 0f32, 0f32);
                for c in 0..80 {
                    let s = sigmoid(logits[[0, q, c]]);
                    if s > best_s {
                        second_s = best_s;
                        best_s = s;
                        best_c = c;
                    } else if s > second_s {
                        second_s = s;
                    }
                }
                if best_s < self.threshold || best_s < self.margin * second_s {
                    continue;
                }
                let label = coco::COCO_LABELS[best_c];
                let Some(cat) = coco::category(label) else {
                    continue;
                };
                let (cx, cy, w, h) = (
                    boxes[[0, q, 0]],
                    boxes[[0, q, 1]],
                    boxes[[0, q, 2]],
                    boxes[[0, q, 3]],
                );
                let bbox = [cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0];
                if !box_ok(label, &bbox) {
                    continue;
                }
                let e = top.entry(cat).or_insert((best_s, bbox));
                if best_s > e.0 {
                    *e = (best_s, bbox);
                }
            }
        }

        // Verifier prob + production gate decision for each category's top candidate (session lock
        // released; the verifier locks its own vision session).
        let mut out = HashMap::new();
        for (cat, (best_raw, bbox)) in top {
            let verifier_prob = match &self.verifier {
                Some(v) => v.confirm_prob(img, &bbox, cat)?,
                None => None,
            };
            let passes = self
                .verifier
                .as_ref()
                .is_none_or(|v| v.accepts(cat, verifier_prob));
            out.insert(
                cat,
                RawScore {
                    best_raw,
                    verifier_prob,
                    gated: best_raw >= coco::threshold(cat) && passes,
                },
            );
        }
        Ok(out)
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

/// Read an `f32` tuning override from the environment, falling back to `default`. Production must NOT
/// set these — they exist only for the offline `presence_eval`/`presence_tune` calibration sweeps.
fn env_f32(key: &str, default: f32) -> f32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Geometric sanity check on a normalized xyxy box. Rejects degenerate noise boxes that survive the
/// score gates: dust specks, near-whole-frame "detections", implausibly wide people, and tiny boxes
/// hugging a frame edge.
fn box_ok(label: &str, b: &[f32; 4]) -> bool {
    let w = (b[2] - b[0]).max(0.0);
    let h = (b[3] - b[1]).max(0.0);
    let area = w * h;
    if !(MIN_AREA..=MAX_AREA).contains(&area) {
        return false;
    }
    if label == "person" && h > 0.0 && w / h > PERSON_MAX_ASPECT {
        return false;
    }
    let touches_edge =
        b[0] <= EDGE_EPS || b[1] <= EDGE_EPS || b[2] >= 1.0 - EDGE_EPS || b[3] >= 1.0 - EDGE_EPS;
    if area < TINY_EDGE_AREA && touches_edge {
        return false;
    }
    true
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
