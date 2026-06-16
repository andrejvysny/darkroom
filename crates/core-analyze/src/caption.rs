//! Image captioning analyzer — Florence-2-base-ft (MIT) via ONNX Runtime + CoreML.
//!
//! Florence-2 is a BART-style seq2seq over [visual tokens ; prompt tokens]. We run the validated
//! 5-session pipeline (see SPIKE.md): vision_encoder → embed_tokens(prompt) → concat → encoder →
//! greedy decode with KV cache (decoder_model first pass, then decoder_with_past_model). All five
//! load at `GraphOptimizationLevel::Level1`; the merged decoder is unusable, so we use the pair.
//!
//! Keywords = caption nouns ∪ detection labels from `prior` (the object-detection record), so the
//! "Detected" panel gets a small searchable tag list without a second model.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use image::RgbImage;
use ort::session::{Session, SessionInputValue};
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::error::AnalyzeError;
use crate::{
    models, preprocess, AnalysisCtx, AnalysisRecord, Analyzer, CaptionPayload, DetectionPayload,
};

const SIZE: u32 = 768;
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];
/// Florence-2 maps the `<CAPTION>` task to this literal prompt (transformers `Florence2Processor`).
const PROMPT: &str = "What does the image describe?";
const DECODER_START: i64 = 2; // </s>
const FORCED_BOS: i64 = 0; // <s> — BART forces this as the first generated token
const EOS: i64 = 2;
const DEFAULT_MAX_TOKENS: usize = 64;

struct Florence2 {
    vision: Session,
    embed: Session,
    encoder: Session,
    decoder: Session,
}

pub struct Captioner {
    inner: Mutex<Florence2>,
    tokenizer: Tokenizer,
    model_version: &'static str,
    max_tokens: usize,
}

/// Owned copy of an f32 tensor output (shape + flat data), decoupled from the session borrow.
struct OwnedF32 {
    shape: Vec<usize>,
    data: Vec<f32>,
}

impl Captioner {
    /// Load the five Florence-2 ONNX components from `dir` plus `tokenizer.json`. All components load
    /// at Level1 optimization (required — see SPIKE.md). `model_version` gates re-analysis.
    pub fn new(
        dir: &Path,
        tokenizer_path: &Path,
        model_version: &'static str,
    ) -> Result<Self, AnalyzeError> {
        let load = |name: &str| models::build_session(&dir.join(name), true);
        let inner = Florence2 {
            vision: load("vision_encoder.onnx")?,
            embed: load("embed_tokens.onnx")?,
            encoder: load("encoder_model.onnx")?,
            decoder: load("decoder_model.onnx")?,
        };
        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| AnalyzeError::Tokenizer(e.to_string()))?;
        Ok(Self {
            inner: Mutex::new(inner),
            tokenizer,
            model_version,
            max_tokens: DEFAULT_MAX_TOKENS,
        })
    }

    /// Generate a short caption for an sRGB image.
    pub fn caption(&self, img: &RgbImage) -> Result<String, AnalyzeError> {
        let mut f = self.inner.lock().expect("caption mutex poisoned");

        // 1. Vision encoder → image features [1, N, 768].
        let pv = t_f32(
            &[1, 3, SIZE as usize, SIZE as usize],
            preprocess::to_chw(img, SIZE, MEAN, STD),
        )?;
        let vout = run_f32(&mut f.vision, vec![named("pixel_values", pv)])?;
        let img_feat = vout
            .get("image_features")
            .ok_or_else(|| miss("image_features"))?;
        let n_img = img_feat.shape[1];

        // 2. Prompt token embeddings [1, T, 768].
        let prompt_ids = self.encode(PROMPT, true)?;
        let t_prompt = prompt_ids.len();
        let eout = run_f32(
            &mut f.embed,
            vec![named(
                "input_ids",
                t_i64(&[1, t_prompt as i64], prompt_ids)?,
            )],
        )?;
        let prompt_embeds = eout
            .get("inputs_embeds")
            .ok_or_else(|| miss("inputs_embeds"))?;

        // 3. Concat [visual ; prompt] embeddings → encoder inputs [1, S, 768]; attention mask = ones.
        let seq = n_img + t_prompt;
        let mut inputs_embeds = Vec::with_capacity(seq * 768);
        inputs_embeds.extend_from_slice(&img_feat.data);
        inputs_embeds.extend_from_slice(&prompt_embeds.data);
        let attn = vec![1i64; seq];
        let encout = run_f32(
            &mut f.encoder,
            vec![
                named("attention_mask", t_i64(&[1, seq as i64], attn.clone())?),
                named("inputs_embeds", t_f32(&[1, seq, 768], inputs_embeds)?),
            ],
        )?;
        let enc_hidden = encout
            .get("last_hidden_state")
            .ok_or_else(|| miss("last_hidden_state"))?;
        let enc_hidden_data = enc_hidden.data.clone();

        // 4. Greedy decode with `decoder_model` (dynamic seq). NOTE: the onnx-community
        // `decoder_with_past_model` export fixes `inputs_embeds` to seq=16, so it can't do 1-token
        // incremental steps; instead we recompute over the full growing sequence each step (no KV
        // cache). Caption lengths are short, so the O(n²) cost is fine for a background pass.
        // Decoder sequence starts with [decoder_start, forced_bos] = [</s>, <s>].
        let mut seq_ids: Vec<i64> = vec![DECODER_START, FORCED_BOS];
        let mut generated: Vec<i64> = Vec::new();
        while generated.len() < self.max_tokens {
            let dec_embeds = self.embed_ids(&mut f.embed, &seq_ids)?;
            let dout = run_f32(
                &mut f.decoder,
                vec![
                    named(
                        "encoder_attention_mask",
                        t_i64(&[1, seq as i64], attn.clone())?,
                    ),
                    named(
                        "encoder_hidden_states",
                        t_f32(&[1, seq, 768], enc_hidden_data.clone())?,
                    ),
                    named("inputs_embeds", dec_embeds),
                ],
            )?;
            let next = argmax_last(dout.get("logits").ok_or_else(|| miss("logits"))?);
            if next == EOS {
                break;
            }
            generated.push(next);
            seq_ids.push(next);
        }
        drop(f);

        let ids_u32: Vec<u32> = generated.iter().map(|&t| t as u32).collect();
        let text = self
            .tokenizer
            .decode(&ids_u32, true)
            .map_err(|e| AnalyzeError::Tokenizer(e.to_string()))?;
        Ok(clean(&text))
    }

    fn encode(&self, text: &str, special: bool) -> Result<Vec<i64>, AnalyzeError> {
        let enc = self
            .tokenizer
            .encode(text, special)
            .map_err(|e| AnalyzeError::Tokenizer(e.to_string()))?;
        Ok(enc.get_ids().iter().map(|&id| id as i64).collect())
    }

    /// Embed token ids → [1, L, 768] via the embed_tokens session.
    fn embed_ids(&self, embed: &mut Session, ids: &[i64]) -> Result<Tensor<f32>, AnalyzeError> {
        let out = run_f32(
            embed,
            vec![named(
                "input_ids",
                t_i64(&[1, ids.len() as i64], ids.to_vec())?,
            )],
        )?;
        let e = out
            .get("inputs_embeds")
            .ok_or_else(|| miss("inputs_embeds"))?;
        t_f32(&e.shape, e.data.clone())
    }
}

impl Analyzer for Captioner {
    fn id(&self) -> &'static str {
        crate::CAPTION_ID
    }

    fn model_version(&self) -> &'static str {
        self.model_version
    }

    fn analyze(&self, ctx: &AnalysisCtx) -> Result<AnalysisRecord, AnalyzeError> {
        let caption = self.caption(ctx.image)?;
        let mut keywords = keywords_from_caption(&caption);
        // Fold in detection labels from a prior object-detection record (deduped, lowercase).
        for rec in ctx.prior {
            if rec.analyzer_id == crate::OBJECT_DETECTION_ID {
                if let Some(p) = rec.parse::<DetectionPayload>() {
                    for d in p.detections {
                        let l = d.label.to_lowercase();
                        if !keywords.contains(&l) {
                            keywords.push(l);
                        }
                    }
                }
            }
        }
        let payload = serde_json::to_value(CaptionPayload { caption, keywords })
            .map_err(|e| AnalyzeError::Other(e.to_string()))?;
        Ok(AnalysisRecord::new(self.id(), self.model_version, payload))
    }
}

// ---- helpers ----

fn miss(name: &str) -> AnalyzeError {
    AnalyzeError::Inference(format!("missing tensor `{name}`"))
}

fn named(
    name: &'static str,
    v: impl Into<SessionInputValue<'static>>,
) -> (Cow<'static, str>, SessionInputValue<'static>) {
    (Cow::Borrowed(name), v.into())
}

fn t_f32(shape: &[usize], data: Vec<f32>) -> Result<Tensor<f32>, AnalyzeError> {
    let shp: Vec<i64> = shape.iter().map(|&d| d as i64).collect();
    Tensor::from_array((shp, data)).map_err(AnalyzeError::inference)
}

fn t_i64(shape: &[i64], data: Vec<i64>) -> Result<Tensor<i64>, AnalyzeError> {
    Tensor::from_array((shape.to_vec(), data)).map_err(AnalyzeError::inference)
}

/// Run a session and copy every f32 output into owned buffers keyed by output name.
fn run_f32(
    sess: &mut Session,
    inputs: Vec<(Cow<'static, str>, SessionInputValue<'static>)>,
) -> Result<HashMap<String, OwnedF32>, AnalyzeError> {
    let names: Vec<String> = sess
        .outputs()
        .iter()
        .map(|o| o.name().to_string())
        .collect();
    let outs = sess.run(inputs).map_err(AnalyzeError::inference)?;
    let mut map = HashMap::with_capacity(names.len());
    for name in names {
        let arr = outs[name.as_str()]
            .try_extract_array::<f32>()
            .map_err(AnalyzeError::inference)?;
        map.insert(
            name,
            OwnedF32 {
                shape: arr.shape().to_vec(),
                data: arr.iter().copied().collect(),
            },
        );
    }
    Ok(map)
}

/// Argmax over the vocabulary at the last sequence position of a `[1, T, vocab]` logits tensor.
fn argmax_last(logits: &OwnedF32) -> i64 {
    let vocab = *logits.shape.last().unwrap_or(&1);
    let t = logits.shape.get(1).copied().unwrap_or(1);
    let start = (t - 1) * vocab;
    let slice = &logits.data[start..start + vocab];
    let mut best = 0usize;
    let mut best_v = f32::NEG_INFINITY;
    for (i, &v) in slice.iter().enumerate() {
        if v > best_v {
            best_v = v;
            best = i;
        }
    }
    best as i64
}

fn clean(s: &str) -> String {
    s.trim()
        .trim_start_matches("</s>")
        .trim_start_matches("<s>")
        .trim()
        .to_string()
}

/// Naive keyword extraction: lowercase, drop punctuation + short/stop words, dedup, cap at 8.
fn keywords_from_caption(caption: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "and", "or", "of", "in", "on", "at", "to", "with", "is", "are", "was",
        "were", "this", "that", "there", "it", "its", "by", "for", "as", "from", "into", "over",
        "image", "picture", "photo", "shows", "showing", "depicts",
    ];
    let mut out: Vec<String> = Vec::new();
    for w in caption.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.len() < 3 || STOP.contains(&w.as_str()) || out.contains(&w) {
            continue;
        }
        out.push(w);
        if out.len() >= 8 {
            break;
        }
    }
    out
}
