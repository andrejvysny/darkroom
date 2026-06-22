//! Face analyzer — composes detection → alignment → embedding into one per-image step.
//!
//! Kept off the [`crate::Analyzer`] trait (which persists to `analysis_results`): face results have a
//! richer, separately-clustered storage (`person`/`face` tables), so the face pass in `src-tauri`
//! calls [`FaceAnalyzer::detect_embed`] directly and writes its own rows.

use std::path::Path;

use image::RgbImage;

use crate::error::AnalyzeError;
use crate::face_aligner::align;
use crate::face_detector::{FaceDetector, Scrfd};
use crate::face_embedder::{ArcFace, FaceEmbedder};

/// Default SCRFD input edge (multiple of 32). 640 is the InsightFace default; raise for small faces.
pub const DEFAULT_FACE_DET_EDGE: u32 = 640;
/// Drop faces whose shorter box edge is below this many source pixels (too small to embed reliably).
const MIN_FACE_PX: f32 = 40.0;

/// One detected, aligned, embedded face. `bbox` normalized `[0,1]`; `kps` in source pixels.
#[derive(Debug, Clone)]
pub struct FaceRecord {
    pub bbox: [f32; 4],
    pub kps: [[f32; 2]; 5],
    pub det_score: f32,
    /// Box pixel-area × aligned-crop sharpness — drives key-photo / cluster-thumbnail selection.
    pub quality: f32,
    /// L2-normalized embedding.
    pub embedding: Vec<f32>,
}

pub struct FaceAnalyzer {
    detector: Box<dyn FaceDetector>,
    embedder: Box<dyn FaceEmbedder>,
}

impl FaceAnalyzer {
    /// Build the default best-accuracy stack: SCRFD-10G detect + ArcFace-512 embed.
    pub fn new(
        detector_path: &Path,
        embedder_path: &Path,
        det_edge: u32,
    ) -> Result<Self, AnalyzeError> {
        Ok(Self {
            detector: Box::new(Scrfd::new(detector_path, det_edge)?),
            embedder: Box::new(ArcFace::new(embedder_path)?),
        })
    }

    pub fn embed_dim(&self) -> usize {
        self.embedder.dim()
    }

    /// Detect every face in `img`, align + embed each one that clears the size floor.
    pub fn detect_embed(&self, img: &RgbImage) -> Result<Vec<FaceRecord>, AnalyzeError> {
        let (ow, oh) = (img.width() as f32, img.height() as f32);
        let mut out = Vec::new();
        for f in self.detector.detect(img)? {
            let w_px = (f.bbox[2] - f.bbox[0]) * ow;
            let h_px = (f.bbox[3] - f.bbox[1]) * oh;
            if w_px.min(h_px) < MIN_FACE_PX {
                continue;
            }
            let aligned = align(img, &f.kps);
            let embedding = self.embedder.embed(&aligned)?;
            let quality = (w_px * h_px) * sharpness(&aligned);
            out.push(FaceRecord {
                bbox: f.bbox,
                kps: f.kps,
                det_score: f.score,
                quality,
                embedding,
            });
        }
        Ok(out)
    }
}

/// Variance of the Laplacian over a grayscale view of the crop — a focus/quality proxy (higher =
/// sharper). Cheap 3×3 4-neighbor Laplacian.
fn sharpness(img: &RgbImage) -> f32 {
    let (w, h) = (img.width() as i64, img.height() as i64);
    if w < 3 || h < 3 {
        return 0.0;
    }
    let lum = |x: i64, y: i64| -> f32 {
        let p = img.get_pixel(x as u32, y as u32);
        0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
    };
    let (mut sum, mut sum_sq, mut count) = (0.0f64, 0.0f64, 0.0f64);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let lap =
                lum(x - 1, y) + lum(x + 1, y) + lum(x, y - 1) + lum(x, y + 1) - 4.0 * lum(x, y);
            sum += lap as f64;
            sum_sq += (lap as f64) * (lap as f64);
            count += 1.0;
        }
    }
    if count == 0.0 {
        return 0.0;
    }
    let mean = sum / count;
    ((sum_sq / count) - mean * mean).max(0.0) as f32
}
