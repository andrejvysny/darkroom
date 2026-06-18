//! Animal detector — MegaDetector v5a (MIT, YOLOv5x6/P6) via ONNX Runtime.
//!
//! Purpose-built wildlife detector (trained on 65M camera-trap frames with vegetation/blank hard
//! negatives), so it covers animals COCO misses (deer, fox, rabbit, …) and rarely fires on texture.
//! We surface only its **animal** class into the Animals bucket (D-FINE keeps People + Vehicles,
//! which it does better on everyday photos).
//!
//! Recipe (community ONNX `bencevans/megadetector-onnx` v0.2.0, dynamic axes):
//! input `images[1,3,S,S]` letterboxed (gray 114 pad, ÷255, RGB); output `output[1,N,5+nc]` =
//! `[cx,cy,w,h, obj, p_animal,p_person,p_vehicle]` in letterboxed **pixel** coords, pre-NMS. Class
//! score = `obj × class_prob`. `S` (640 or 1280) is configurable.

use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, Mutex};

use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::error::AnalyzeError;
use crate::{
    models, preprocess, AnalysisCtx, AnalysisRecord, Analyzer, Detection, DetectionPayload,
    RawScore, Verifier,
};

/// MegaDetector class indices (0-indexed in the tensor).
const CLS_ANIMAL: usize = 0;
const DEFAULT_THRESHOLD: f32 = 0.35; // candidate score (obj×cls); the CLIP verifier confirms survivors
const NMS_IOU: f32 = 0.45;

pub struct MegaDetector {
    session: Mutex<Session>,
    model_version: &'static str,
    input_size: u32,
    threshold: f32,
    nms_iou: f32,
    verifier: Option<Arc<Verifier>>,
}

impl MegaDetector {
    /// Load the MegaDetector ONNX. `input_size` (640 or 1280) is the letterbox target; the model has
    /// dynamic input axes so a single file serves both. `model_version` should encode the size so a
    /// resolution change re-analyzes. The candidate threshold and NMS IoU take their defaults from
    /// [`DEFAULT_THRESHOLD`]/[`NMS_IOU`], overridable for offline calibration only via
    /// `DARKROOM_MD_THRESHOLD`/`DARKROOM_MD_NMS_IOU` (production must NOT set these).
    pub fn new(
        model_path: &Path,
        model_version: &'static str,
        input_size: u32,
    ) -> Result<Self, AnalyzeError> {
        // The ONNX has dynamic spatial axes, but we always feed a FIXED `[1,3,S,S]` (S = input_size),
        // so CoreML MLProgram + static_input_shapes can compile it onto the ANE/GPU — the same recipe
        // D-FINE uses (see `models::build_session`). MLProgram keeps FP32-ish intermediates, so scores
        // stay deterministic for thresholding. `DARKROOM_MD_CPU=1` forces the CPU EP (A/B benchmarking,
        // or a machine where CoreML mishandles a YOLOv5x6 op and partial fallback shifts scores).
        let session = if std::env::var_os("DARKROOM_MD_CPU").is_some() {
            models::build_session_cpu(model_path)?
        } else {
            models::build_session(model_path, false, true)?
        };
        Ok(Self {
            session: Mutex::new(session),
            model_version,
            input_size,
            threshold: env_f32("DARKROOM_MD_THRESHOLD", DEFAULT_THRESHOLD),
            nms_iou: env_f32("DARKROOM_MD_NMS_IOU", NMS_IOU),
            verifier: None,
        })
    }

    pub fn with_verifier(mut self, verifier: Arc<Verifier>) -> Self {
        self.verifier = Some(verifier);
        self
    }

    /// Detect animals in an sRGB image. Boxes returned normalized `[0,1]` to the source image.
    pub fn detect(&self, img: &RgbImage) -> Result<Vec<Detection>, AnalyzeError> {
        let s = self.input_size;
        let (chw, scale, pad_x, pad_y) = preprocess::to_letterbox_chw(img, s);
        let input = Tensor::from_array(([1usize, 3, s as usize, s as usize], chw))
            .map_err(AnalyzeError::inference)?;

        let mut session = self.session.lock().expect("megadetector mutex poisoned");
        let in_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .unwrap_or_else(|| "images".into());
        let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> =
            vec![(Cow::Owned(in_name), input.into())];
        let outputs = session.run(inputs).map_err(AnalyzeError::inference)?;
        let arr = outputs[0]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;
        // Expect [1, N, 5+nc].
        let shape = arr.shape();
        if shape.len() != 3 || shape[2] < 6 {
            return Err(AnalyzeError::Inference(format!(
                "unexpected MegaDetector output shape {shape:?}"
            )));
        }
        let (n, cols) = (shape[1], shape[2]);
        let nc = cols - 5;

        let (ow, oh) = (img.width() as f32, img.height() as f32);
        let content_w = ow * scale; // letterboxed content size (excluding padding)
        let content_h = oh * scale;

        let mut dets = Vec::new();
        for i in 0..n {
            let obj = arr[[0, i, 4]];
            if obj < self.threshold {
                continue;
            }
            // Argmax over the nc class probs; keep only when "animal" wins.
            let (mut best_c, mut best_p) = (0usize, 0f32);
            for c in 0..nc {
                let p = arr[[0, i, 5 + c]];
                if p > best_p {
                    best_p = p;
                    best_c = c;
                }
            }
            if best_c != CLS_ANIMAL {
                continue;
            }
            let score = obj * best_p;
            if score < self.threshold {
                continue;
            }
            // Box: letterboxed-pixel cxcywh → xyxy → undo pad/scale → normalized to source image.
            let (cx, cy, w, h) = (
                arr[[0, i, 0]],
                arr[[0, i, 1]],
                arr[[0, i, 2]],
                arr[[0, i, 3]],
            );
            let x0 = (((cx - w / 2.0) - pad_x) / content_w).clamp(0.0, 1.0);
            let y0 = (((cy - h / 2.0) - pad_y) / content_h).clamp(0.0, 1.0);
            let x1 = (((cx + w / 2.0) - pad_x) / content_w).clamp(0.0, 1.0);
            let y1 = (((cy + h / 2.0) - pad_y) / content_h).clamp(0.0, 1.0);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            dets.push(Detection {
                label: "animal".to_string(),
                category: "Animals".to_string(),
                confidence: score,
                bbox: [x0, y0, x1, y1],
            });
        }
        let mut kept = nms(dets, self.nms_iou);

        // CLIP crop re-check (if attached) — drops confident-but-wrong animal boxes.
        if let Some(v) = &self.verifier {
            let mut confirmed = Vec::with_capacity(kept.len());
            for d in kept.drain(..) {
                if v.confirm(img, &d.bbox, &d.category)? {
                    confirmed.push(d);
                }
            }
            kept = confirmed;
        }
        Ok(kept)
    }

    /// Diagnostic **pre-gate** animal score for offline threshold sweeping (NOT production). Mirrors
    /// [`detect`](Self::detect) but DROPS the two `< self.threshold` guards so sub-0.35 candidates are
    /// still measured; tracks the single highest-scoring valid animal box (equivalent to NMS's top
    /// survivor) and captures its verifier prob and the decision production `detect()` would make
    /// (`gated = best_raw ≥ threshold && verifier accepts`).
    pub fn score_raw(&self, img: &RgbImage) -> Result<RawScore, AnalyzeError> {
        let s = self.input_size;
        let (chw, scale, pad_x, pad_y) = preprocess::to_letterbox_chw(img, s);
        let input = Tensor::from_array(([1usize, 3, s as usize, s as usize], chw))
            .map_err(AnalyzeError::inference)?;

        // Best animal candidate (score, bbox), captured under the session lock.
        let mut best: Option<(f32, [f32; 4])> = None;
        {
            let mut session = self.session.lock().expect("megadetector mutex poisoned");
            let in_name = session
                .inputs()
                .first()
                .map(|i| i.name().to_string())
                .unwrap_or_else(|| "images".into());
            let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> =
                vec![(Cow::Owned(in_name), input.into())];
            let outputs = session.run(inputs).map_err(AnalyzeError::inference)?;
            let arr = outputs[0]
                .try_extract_array::<f32>()
                .map_err(AnalyzeError::inference)?;
            let shape = arr.shape();
            if shape.len() != 3 || shape[2] < 6 {
                return Err(AnalyzeError::Inference(format!(
                    "unexpected MegaDetector output shape {shape:?}"
                )));
            }
            let (n, cols) = (shape[1], shape[2]);
            let nc = cols - 5;
            let (ow, oh) = (img.width() as f32, img.height() as f32);
            let content_w = ow * scale;
            let content_h = oh * scale;
            for i in 0..n {
                let obj = arr[[0, i, 4]];
                // NO score threshold here (diagnostic): production drops candidates < self.threshold.
                let (mut best_c, mut best_p) = (0usize, 0f32);
                for c in 0..nc {
                    let p = arr[[0, i, 5 + c]];
                    if p > best_p {
                        best_p = p;
                        best_c = c;
                    }
                }
                if best_c != CLS_ANIMAL {
                    continue;
                }
                let score = obj * best_p;
                let (cx, cy, w, h) = (
                    arr[[0, i, 0]],
                    arr[[0, i, 1]],
                    arr[[0, i, 2]],
                    arr[[0, i, 3]],
                );
                let x0 = (((cx - w / 2.0) - pad_x) / content_w).clamp(0.0, 1.0);
                let y0 = (((cy - h / 2.0) - pad_y) / content_h).clamp(0.0, 1.0);
                let x1 = (((cx + w / 2.0) - pad_x) / content_w).clamp(0.0, 1.0);
                let y1 = (((cy + h / 2.0) - pad_y) / content_h).clamp(0.0, 1.0);
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                if best.is_none_or(|(bs, _)| score > bs) {
                    best = Some((score, [x0, y0, x1, y1]));
                }
            }
        }

        // Verifier prob + production gate decision on the top candidate (session lock released).
        let (best_raw, verifier_prob) = match best {
            Some((score, bbox)) => {
                let vp = match &self.verifier {
                    Some(v) => v.confirm_prob(img, &bbox, "Animals")?,
                    None => None,
                };
                (score, vp)
            }
            None => (0.0, None),
        };
        let passes = self
            .verifier
            .as_ref()
            .is_none_or(|v| v.accepts(verifier_prob));
        Ok(RawScore {
            best_raw,
            verifier_prob,
            gated: best.is_some() && best_raw >= self.threshold && passes,
        })
    }
}

impl Analyzer for MegaDetector {
    fn id(&self) -> &'static str {
        crate::ANIMAL_DETECTION_ID
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

/// Read an `f32` tuning override from the environment, falling back to `default` (sweep-only).
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

/// Greedy non-max suppression (single class) — highest confidence wins.
fn nms(mut dets: Vec<Detection>, iou_thresh: f32) -> Vec<Detection> {
    dets.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut keep: Vec<Detection> = Vec::new();
    'outer: for d in dets {
        for k in &keep {
            if iou(&k.bbox, &d.bbox) > iou_thresh {
                continue 'outer;
            }
        }
        keep.push(d);
    }
    keep
}
