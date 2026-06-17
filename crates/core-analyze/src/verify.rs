//! Detection verifier — MobileCLIP-S1 (Apple, MIT) zero-shot crop re-check via ONNX Runtime.
//!
//! D-FINE / MegaDetector emit confident-but-wrong boxes on texture (e.g. a red poppy scored
//! `person@0.83`). A CLIP crop re-check is architecturally uncorrelated with the detector, so a joint
//! false positive on the same texture is unlikely. For each candidate we crop the box (with padding),
//! embed it, and compare against a fixed positive/negative prompt set for its category; we keep the
//! detection only if the positive prompt wins by enough.
//!
//! Model: `Xenova/mobileclip_s1` ONNX — separate vision (`pixel_values[1,3,256,256]→image_embeds[1,512]`)
//! and text (`input_ids/attention_mask[1,77]→text_embeds[1,512]`) graphs; CLIP BPE tokenizer; embeds
//! are L2-normalized, identity image normalization (`÷255` only). Text embeds are precomputed once.

use std::borrow::Cow;
use std::path::Path;
use std::sync::Mutex;

use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer};

use crate::error::AnalyzeError;
use crate::{models, preprocess};

const CLIP_INPUT: u32 = 256;
/// CLIP text context length — the text encoder bakes 77 positional embeddings, so `input_ids` must be
/// exactly this length (padded with id 0; EOS-argmax pooling is unaffected).
const CONTEXT_LEN: usize = 77;
const LOGIT_SCALE: f32 = 100.0;
/// Crop padding fraction around the detection box (context helps CLIP).
const CROP_PAD: f32 = 0.20;
/// Keep a detection only if the positive prompt's softmax probability is at least this. Conservative
/// (recall-preserving): we reject only when the negatives clearly dominate.
const VERIFY_ACCEPT: f32 = 0.40;

/// A category's verification prompts. Index 0 is the positive; the rest are negatives.
struct PromptSet {
    /// L2-normalized text embeddings, positive first.
    embeds: Vec<Vec<f32>>,
}

fn people_prompts() -> Vec<&'static str> {
    vec![
        "a photo of a person",
        "a landscape photograph",
        "a close-up of a flower",
        "a plant",
        "an abstract pattern",
        "an empty scene with no people",
    ]
}

fn animal_prompts() -> Vec<&'static str> {
    vec![
        "a photo of an animal",
        "a landscape photograph",
        "a plant",
        "a man-made object",
        "an abstract pattern",
        "an empty scene with no animals",
    ]
}

pub struct Verifier {
    vision: Mutex<Session>,
    people: PromptSet,
    animals: PromptSet,
}

impl Verifier {
    /// Load the MobileCLIP vision + text ONNX graphs and the tokenizer, then precompute the text
    /// prompt embeddings (the text session is dropped afterwards — only the vision graph is kept).
    pub fn new(
        vision_path: &Path,
        text_path: &Path,
        tokenizer_path: &Path,
    ) -> Result<Self, AnalyzeError> {
        let vision = models::build_session(vision_path, false, false)?;
        // Text encoder on CPU: its dynamic sequence length isn't resizable by the CoreML EP, and it
        // only runs once per prompt at startup.
        let mut text = models::build_session_cpu(text_path)?;
        // CLIP BPE tokenizer; the text graph takes `input_ids` only (EOS pooling is internal) but
        // requires a fixed 77-token context, so right-pad to CONTEXT_LEN with id 0.
        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| AnalyzeError::Tokenizer(e.to_string()))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::Fixed(CONTEXT_LEN),
            ..Default::default()
        }));

        let people = PromptSet {
            embeds: embed_prompts(&mut text, &tokenizer, &people_prompts())?,
        };
        let animals = PromptSet {
            embeds: embed_prompts(&mut text, &tokenizer, &animal_prompts())?,
        };
        Ok(Self {
            vision: Mutex::new(vision),
            people,
            animals,
        })
    }

    /// True if the (normalized) box, cropped from `img`, is confirmed as `category`. Categories without
    /// a prompt set (e.g. Vehicles) are accepted unverified.
    pub fn confirm(
        &self,
        img: &RgbImage,
        bbox: &[f32; 4],
        category: &str,
    ) -> Result<bool, AnalyzeError> {
        let prompts = match category {
            "People" => &self.people,
            "Animals" => &self.animals,
            _ => return Ok(true),
        };
        let crop = crop_padded(img, bbox);
        let emb = self.embed_image(&crop)?;
        // Cosine (embeds are unit-norm) → scaled logits → softmax; positive is index 0.
        let logits: Vec<f32> = prompts
            .embeds
            .iter()
            .map(|t| LOGIT_SCALE * dot(&emb, t))
            .collect();
        let probs = softmax(&logits);
        Ok(probs[0] >= VERIFY_ACCEPT)
    }

    fn embed_image(&self, crop: &RgbImage) -> Result<Vec<f32>, AnalyzeError> {
        let chw = preprocess::to_clip_chw(crop, CLIP_INPUT);
        let input =
            Tensor::from_array(([1usize, 3, CLIP_INPUT as usize, CLIP_INPUT as usize], chw))
                .map_err(AnalyzeError::inference)?;
        let mut sess = self.vision.lock().expect("verifier vision mutex poisoned");
        let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> =
            vec![(Cow::Borrowed("pixel_values"), input.into())];
        let outputs = sess.run(inputs).map_err(AnalyzeError::inference)?;
        let mut v = first_f32(&outputs)?;
        l2_normalize(&mut v);
        Ok(v)
    }
}

/// Embed each text prompt → unit-norm row. Reuses one (input_ids, attention_mask) pair per prompt.
fn embed_prompts(
    text: &mut Session,
    tokenizer: &Tokenizer,
    prompts: &[&str],
) -> Result<Vec<Vec<f32>>, AnalyzeError> {
    let mut out = Vec::with_capacity(prompts.len());
    for p in prompts {
        let enc = tokenizer
            .encode(*p, true)
            .map_err(|e| AnalyzeError::Tokenizer(e.to_string()))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&i| i as i64).collect();
        let n = ids.len() as i64;
        let id_t = Tensor::from_array(([1i64, n], ids)).map_err(AnalyzeError::inference)?;
        let inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)> =
            vec![(Cow::Borrowed("input_ids"), id_t.into())];
        let outputs = text.run(inputs).map_err(AnalyzeError::inference)?;
        let mut v = first_f32(&outputs)?;
        l2_normalize(&mut v);
        out.push(v);
    }
    Ok(out)
}

/// Extract the first f32 output tensor as an owned flat Vec.
fn first_f32(outputs: &ort::session::SessionOutputs<'_>) -> Result<Vec<f32>, AnalyzeError> {
    let arr = outputs[0]
        .try_extract_array::<f32>()
        .map_err(AnalyzeError::inference)?;
    Ok(arr.iter().copied().collect())
}

/// Crop the normalized box from `img`, padded by `CROP_PAD` and clamped to bounds.
fn crop_padded(img: &RgbImage, b: &[f32; 4]) -> RgbImage {
    let (iw, ih) = (img.width() as f32, img.height() as f32);
    let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
    let x0 = ((b[0] - CROP_PAD * bw).clamp(0.0, 1.0) * iw) as u32;
    let y0 = ((b[1] - CROP_PAD * bh).clamp(0.0, 1.0) * ih) as u32;
    let x1 = ((b[2] + CROP_PAD * bw).clamp(0.0, 1.0) * iw) as u32;
    let y1 = ((b[3] + CROP_PAD * bh).clamp(0.0, 1.0) * ih) as u32;
    let w = x1
        .saturating_sub(x0)
        .max(1)
        .min(img.width() - x0.min(img.width() - 1));
    let h = y1
        .saturating_sub(y0)
        .max(1)
        .min(img.height() - y0.min(img.height() - 1));
    image::imageops::crop_imm(img, x0, y0, w, h).to_image()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn l2_normalize(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x /= n;
        }
    }
}

fn softmax(logits: &[f32]) -> Vec<f32> {
    let m = logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let exps: Vec<f32> = logits.iter().map(|&l| (l - m).exp()).collect();
    let sum: f32 = exps.iter().sum();
    exps.iter().map(|&e| e / sum).collect()
}
