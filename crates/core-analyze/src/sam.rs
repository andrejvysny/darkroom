//! Interactive segmentation (Lightroom-style AI masking) — MobileSAM via ONNX Runtime.
//!
//! Split encode/decode, the standard SAM design:
//! - **Encoder** (heavy TinyViT, run once per image, embedding cached): `input_image` HWC f32 RGB
//!   0–255 with the longest side resized to [`SAM_EDGE`] (the graph normalizes / permutes / zero-pads
//!   to 1024² internally) → `image_embeddings [1,256,64,64]`.
//! - **Decoder** (tiny, run per click): `image_embeddings` + `point_coords` (in the working frame) +
//!   `point_labels` (1 fg / 0 bg / -1 pad / 2,3 box corners) + `mask_input`/`has_mask_input` (256²
//!   feedback for refinement) + `orig_im_size` → `masks [1,C,H,W]` logits (>0), `iou_predictions
//!   [1,C]`, `low_res_masks [1,C,256,256]`. Pick the argmax-IoU mask.
//!
//! The returned alpha is at the SAM working resolution (longest side [`SAM_EDGE`]); it is sampled by
//! normalized UV on the GPU and edge-refined against the full-res image, so it need not be full-res.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Mutex;

use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;

use crate::error::AnalyzeError;
use crate::{models, preprocess};

/// Longest-side working resolution for the SAM pipeline. SAM encoders are trained at 1024; masks come
/// back at this size and are refined against the full-res image on the GPU.
pub const SAM_EDGE: u32 = 1024;

/// Image-encoder input convention, which differs by export family (the decoder is identical):
/// - `HwcBaked`: the Acly **MobileSAM** export bakes resize/normalize/pad into the graph — feed a raw
///   HWC RGB 0–255 buffer with the longest side already 1024.
/// - `Chw`: the standard **SAM ViT-B/L** (samexporter) export takes a fully pre-processed
///   `[1,3,1024,1024]` CHW tensor (resize-longest + normalize + zero-pad done by the caller).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamPreproc {
    HwcBaked,
    Chw,
}

/// A cached per-image encoder embedding plus the working-image dims it was computed at (needed to map
/// normalized prompt points into the decoder's coordinate frame).
#[derive(Clone)]
pub struct SamEmbedding {
    /// `[1,256,64,64]` row-major.
    pub data: Vec<f32>,
    pub work_w: u32,
    pub work_h: u32,
}

/// A click prompt in **normalized `[0,1]`** image coords. `positive=false` subtracts (background).
#[derive(Clone, Copy, Debug)]
pub struct Prompt {
    pub x: f32,
    pub y: f32,
    pub positive: bool,
}

/// The decoded mask: `alpha` is `width*height` bytes (0/255), row-major, at the working resolution.
pub struct SamMask {
    pub alpha: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub iou: f32,
    /// Best mask token at 256² — feed back as `mask_input` to refine on the next prompt.
    pub low_res: Vec<f32>,
}

/// Owns the encoder + decoder sessions. Held on app state; `encode` once per image, `decode` per click.
pub struct Segmenter {
    encoder: Mutex<Session>,
    decoder: Mutex<Session>,
    preproc: SamPreproc,
}

impl Segmenter {
    /// Load a SAM encoder + decoder ONNX. `preproc` selects the encoder input convention (per tier —
    /// see [`SamPreproc`]). The encoder input is dynamic-shape (per-image resized dims for MobileSAM;
    /// fixed 1024² but still not MLProgram-friendly for ViT), so it uses the NeuralNetwork CoreML
    /// backend (`mlprogram=false`) with graceful per-op CPU fallback. The decoder has a dynamic point
    /// count → CPU (a few-MB model, sub-millisecond per run).
    pub fn new(
        encoder_path: &Path,
        decoder_path: &Path,
        preproc: SamPreproc,
        encoder_cpu: bool,
    ) -> Result<Self, AnalyzeError> {
        // Encoder execution strategy (per tier):
        // - `encoder_cpu`: CPU only. For the huge ViT-L (1.23 GB, dynamic HWC) the CoreML
        //   NeuralNetwork backend errors instead of falling back; CPU runs it reliably (~3.4 s).
        // - ViT-B (`Chw`, FIXED 1024²): CoreML MLProgram + static input shapes → GPU/ANE.
        // - MobileSAM (`HwcBaked`, small dynamic): CoreML NeuralNetwork (no static shapes).
        let encoder = if encoder_cpu {
            models::build_session_cpu(encoder_path)?
        } else {
            models::build_session(encoder_path, false, matches!(preproc, SamPreproc::Chw))?
        };
        let decoder = models::build_session_cpu(decoder_path)?;
        Ok(Self {
            encoder: Mutex::new(encoder),
            decoder: Mutex::new(decoder),
            preproc,
        })
    }

    /// Run the image encoder once (the heavy step). Feed the oriented preview; it is downscaled to
    /// [`SAM_EDGE`] longest side here. The result should be cached per image.
    pub fn encode(&self, img: &RgbImage) -> Result<SamEmbedding, AnalyzeError> {
        // Both conventions carry the same working dims `(w,h)` (longest side = SAM_EDGE); only the
        // encoder input tensor layout differs.
        let (input, w, h) = match self.preproc {
            SamPreproc::HwcBaked => {
                let (hwc, w, h) = preprocess::to_sam_hwc(img, SAM_EDGE);
                let t = Tensor::from_array(([h as usize, w as usize, 3usize], hwc))
                    .map_err(AnalyzeError::inference)?;
                (t, w, h)
            }
            SamPreproc::Chw => {
                let (chw, w, h) = preprocess::to_sam_chw(img, SAM_EDGE);
                let e = SAM_EDGE as usize;
                let t = Tensor::from_array(([1usize, 3, e, e], chw))
                    .map_err(AnalyzeError::inference)?;
                (t, w, h)
            }
        };
        let mut sess = self.encoder.lock().expect("sam encoder mutex poisoned");
        let outputs = sess
            .run(ort::inputs![input])
            .map_err(AnalyzeError::inference)?;
        let arr = outputs["image_embeddings"]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;
        let data: Vec<f32> = arr.iter().copied().collect();
        Ok(SamEmbedding {
            data,
            work_w: w,
            work_h: h,
        })
    }

    /// Decode a mask from prompts against a cached embedding. `points` are normalized `[0,1]`;
    /// `box_norm` is optional normalized xyxy; `prev_low_res` (256²) feeds edge refinement across
    /// successive add/subtract clicks. Returns the best-IoU mask.
    pub fn decode(
        &self,
        emb: &SamEmbedding,
        points: &[Prompt],
        box_norm: Option<[f32; 4]>,
        prev_low_res: Option<&[f32]>,
    ) -> Result<SamMask, AnalyzeError> {
        let (w, h) = (emb.work_w as f32, emb.work_h as f32);
        let mut coords: Vec<f32> = Vec::new();
        let mut labels: Vec<f32> = Vec::new();
        if let Some(b) = box_norm {
            // Box encoded as two points: top-left (label 2), bottom-right (label 3).
            coords.extend_from_slice(&[b[0] * w, b[1] * h, b[2] * w, b[3] * h]);
            labels.extend_from_slice(&[2.0, 3.0]);
        }
        for p in points {
            coords.push(p.x * w);
            coords.push(p.y * h);
            labels.push(if p.positive { 1.0 } else { 0.0 });
        }
        // SAM appends a single padding point (0,0)/-1 when there is no box prompt.
        if box_norm.is_none() {
            coords.extend_from_slice(&[0.0, 0.0]);
            labels.push(-1.0);
        }
        let n = labels.len();

        let (mask_input, has_mask) = match prev_low_res {
            Some(m) if m.len() == 256 * 256 => (m.to_vec(), 1.0f32),
            _ => (vec![0f32; 256 * 256], 0.0f32),
        };

        let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> = vec![
            (
                Cow::Borrowed("image_embeddings"),
                Tensor::from_array(([1usize, 256, 64, 64], emb.data.clone()))
                    .map_err(AnalyzeError::inference)?
                    .into(),
            ),
            (
                Cow::Borrowed("point_coords"),
                Tensor::from_array(([1usize, n, 2], coords))
                    .map_err(AnalyzeError::inference)?
                    .into(),
            ),
            (
                Cow::Borrowed("point_labels"),
                Tensor::from_array(([1usize, n], labels))
                    .map_err(AnalyzeError::inference)?
                    .into(),
            ),
            (
                Cow::Borrowed("mask_input"),
                Tensor::from_array(([1usize, 1, 256, 256], mask_input))
                    .map_err(AnalyzeError::inference)?
                    .into(),
            ),
            (
                Cow::Borrowed("has_mask_input"),
                Tensor::from_array(([1usize], vec![has_mask]))
                    .map_err(AnalyzeError::inference)?
                    .into(),
            ),
            (
                Cow::Borrowed("orig_im_size"),
                Tensor::from_array(([2usize], vec![emb.work_h as f32, emb.work_w as f32]))
                    .map_err(AnalyzeError::inference)?
                    .into(),
            ),
        ];

        let mut sess = self.decoder.lock().expect("sam decoder mutex poisoned");
        let outs = sess.run(inputs).map_err(AnalyzeError::inference)?;
        let masks = outs["masks"]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;
        let iou = outs["iou_predictions"]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;
        let low = outs["low_res_masks"]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;

        // masks [1,C,H,W], iou [1,C], low [1,C,256,256] — pick the highest-IoU mask token. Outputs
        // are C-contiguous (standard layout), so slice out the chosen token's plane directly rather
        // than paying dynamic-stride ndarray indexing per pixel (that dominated decode latency).
        let mshape = masks.shape();
        let (mc, mh, mw) = (mshape[1], mshape[2], mshape[3]);
        let mut best = 0usize;
        let mut best_iou = f32::NEG_INFINITY;
        for c in 0..mc {
            let v = iou[[0, c]];
            if v > best_iou {
                best_iou = v;
                best = c;
            }
        }
        let plane = mh * mw;
        let masks_flat = masks
            .as_slice()
            .ok_or_else(|| AnalyzeError::Inference("masks not contiguous".into()))?;
        let mstart = best * plane;
        let mut alpha: Vec<u8> = masks_flat[mstart..mstart + plane]
            .iter()
            .map(|&v| if v > 0.0 { 255 } else { 0 })
            .collect();
        // Drop stray blobs: keep only mask regions connected to a positive click (SAM occasionally
        // emits disconnected specks). Seeds are the positive points in mask-pixel coords.
        let seeds: Vec<(usize, usize)> = points
            .iter()
            .filter(|p| p.positive)
            .map(|p| {
                (
                    ((p.x * mw as f32) as usize).min(mw.saturating_sub(1)),
                    ((p.y * mh as f32) as usize).min(mh.saturating_sub(1)),
                )
            })
            .collect();
        keep_connected(&mut alpha, mw, mh, &seeds);
        let low_flat = low
            .as_slice()
            .ok_or_else(|| AnalyzeError::Inference("low_res_masks not contiguous".into()))?;
        let lstart = best * 256 * 256;
        let low_res = low_flat[lstart..lstart + 256 * 256].to_vec();
        Ok(SamMask {
            alpha,
            width: mw as u32,
            height: mh as u32,
            iou: best_iou,
            low_res,
        })
    }
}

/// Zero every mask pixel not 4-connected to a `seeds` pixel (over `alpha>0`). No-op when there are no
/// seeds or none land on the mask (so a click that just misses the edge never blanks the whole mask).
fn keep_connected(alpha: &mut [u8], w: usize, h: usize, seeds: &[(usize, usize)]) {
    let mut stack: Vec<usize> = seeds
        .iter()
        .filter(|&&(x, y)| x < w && y < h && alpha[y * w + x] > 0)
        .map(|&(x, y)| y * w + x)
        .collect();
    if stack.is_empty() {
        return;
    }
    let mut keep = vec![false; w * h];
    for &i in &stack {
        keep[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        let mut visit = |nx: usize, ny: usize, st: &mut Vec<usize>| {
            let j = ny * w + nx;
            if alpha[j] > 0 && !keep[j] {
                keep[j] = true;
                st.push(j);
            }
        };
        if x > 0 {
            visit(x - 1, y, &mut stack);
        }
        if x + 1 < w {
            visit(x + 1, y, &mut stack);
        }
        if y > 0 {
            visit(x, y - 1, &mut stack);
        }
        if y + 1 < h {
            visit(x, y + 1, &mut stack);
        }
    }
    for (i, a) in alpha.iter_mut().enumerate() {
        if !keep[i] {
            *a = 0;
        }
    }
}
