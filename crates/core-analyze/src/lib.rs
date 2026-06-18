//! core-analyze — pluggable per-image analysis engine.
//!
//! Runs offline AI analyzers (object detection, image captioning) during the library scan's
//! background analysis pass. ONNX inference via [`ort`] (ONNX Runtime) with the CoreML execution
//! provider on Apple Silicon; kept independent of the app's `wgpu` Metal usage.
//!
//! Adding a new analyzer = implement [`Analyzer`] and register it — the scan pass and storage are
//! generic over the trait. See `SPIKE.md` for the validated model recipes and ORT gotchas.

pub mod caption;
pub mod coco;
pub mod detector;
pub mod error;
pub mod megadetector;
pub mod metrics;
pub mod models;
pub mod presence;
mod preprocess;
pub mod verify;

use std::sync::Arc;

pub use caption::Captioner;
pub use detector::ObjectDetector;
pub use error::AnalyzeError;
pub use megadetector::MegaDetector;
/// Re-export so downstream crates link the exact same `ort`.
pub use ort;
pub use presence::PresenceProbe;
pub use verify::Verifier;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Stable analyzer ids (also the `analyzer_id` stored per result row).
pub const OBJECT_DETECTION_ID: &str = "object_detection";
pub const ANIMAL_DETECTION_ID: &str = "animal_detection";
pub const CAPTION_ID: &str = "caption";
pub const PRESENCE_ID: &str = "presence_probe";

/// Per-image input handed to each analyzer. sRGB pixels are already decoded (analyzers resize as
/// needed). `prior` holds the records produced by earlier analyzers for this same image, so a later
/// stage can read them without a DB round-trip (e.g. the captioner folds detection labels into keywords).
pub struct AnalysisCtx<'a> {
    pub image_id: i64,
    pub content_hash_hex: &'a str,
    pub image: &'a image::RgbImage,
    pub prior: &'a [AnalysisRecord],
}

/// One analyzer's result for one image. `payload` is a typed-per-analyzer JSON value
/// ([`DetectionPayload`], [`CaptionPayload`], …) — the canonical record persisted to `analysis_results`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRecord {
    pub analyzer_id: String,
    pub model_version: String,
    pub payload: serde_json::Value,
}

impl AnalysisRecord {
    pub fn new(analyzer_id: &str, model_version: &str, payload: serde_json::Value) -> Self {
        Self {
            analyzer_id: analyzer_id.to_string(),
            model_version: model_version.to_string(),
            payload,
        }
    }

    /// Decode the payload into a typed struct (returns `None` on shape mismatch).
    pub fn parse<T: DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.payload.clone()).ok()
    }
}

/// A single detected object, box in original-image pixel coords `[x0, y0, x1, y1]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Detection {
    pub label: String,
    pub category: String, // People | Animals | Vehicles
    pub confidence: f32,
    pub bbox: [f32; 4],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectionPayload {
    pub detections: Vec<Detection>,
}

/// Diagnostic per-image scores for offline threshold / PR sweeping (NOT persisted). `best_raw` is the
/// best candidate score for a category BEFORE the per-category accept threshold (D-FINE: sigmoid;
/// MegaDetector: `obj×cls`), but after the absolute floor / margin / box-sanity that define a real
/// candidate. `verifier_prob` is the CLIP positive-prompt softmax for that top candidate (`None` if
/// there is no candidate, no verifier, or the category has no prompt set). `gated` is the decision the
/// production `detect()` would make for this category.
#[derive(Debug, Clone, Default)]
pub struct RawScore {
    pub best_raw: f32,
    pub verifier_prob: Option<f32>,
    pub gated: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CaptionPayload {
    pub caption: String,
    pub keywords: Vec<String>,
}

/// MobileCLIP linear-probe presence scores in `[0,1]` — `p_person`/`p_animal` are calibrated
/// `sigmoid(w·embedding + b)` per category, fused with the detectors (OR at the baked threshold).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresencePayload {
    pub p_person: f32,
    pub p_animal: f32,
}

/// An analyzer stage. Object-safe (no generics/associated types) so it can live behind `dyn`.
/// `model_version` is a stable string bumped when the model/behavior changes — it gates incremental
/// re-analysis (a new version re-runs; an unchanged one is skipped).
pub trait Analyzer: Send + Sync {
    fn id(&self) -> &'static str;
    fn model_version(&self) -> &'static str;
    fn analyze(&self, ctx: &AnalysisCtx) -> Result<AnalysisRecord, AnalyzeError>;
}

/// Ordered set of analyzers, built once at startup. Order matters: later analyzers see earlier
/// results via [`AnalysisCtx::prior`].
#[derive(Default)]
pub struct AnalyzerRegistry {
    analyzers: Vec<Arc<dyn Analyzer>>,
}

impl AnalyzerRegistry {
    pub fn new() -> Self {
        Self {
            analyzers: Vec::new(),
        }
    }

    pub fn register(&mut self, a: Arc<dyn Analyzer>) {
        self.analyzers.push(a);
    }

    pub fn analyzers(&self) -> &[Arc<dyn Analyzer>] {
        &self.analyzers
    }

    pub fn is_empty(&self) -> bool {
        self.analyzers.is_empty()
    }
}
