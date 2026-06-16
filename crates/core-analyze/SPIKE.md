# core-analyze — Phase 0 spike results (validated 2026-06-16)

De-risking for embedded object detection + captioning via `ort` (ONNX Runtime) + CoreML on Apple
Silicon. All findings validated on this machine (M-series, rustc 1.91, ort 2.0.0-rc.12, onnxruntime
prebuilt). Harnesses: `examples/detect_one.rs`, `examples/onnx_io.rs`.

## Runtime — VALIDATED ✅

- `ort = { version = "=2.0.0-rc.12", features = ["coreml"] }` builds on rustc 1.91; default
  `download-binaries` fetches a CoreML-capable prebuilt onnxruntime (~21s build, no source compile).
- CoreML EP **registers and runs**: proven with `ort::ep::CoreML::default().with_compute_units(All)
.build().error_on_failure()` — session builds (would hard-fail if CoreML were unavailable).
- `ndarray ^0.17` and `image ^0.25` already match the workspace — no version split.

## Object detection — VALIDATED ✅ (production-ready recipe)

- Model: **D-FINE** (Apache-2.0), ready ONNX at `onnx-community/dfine_{n,s,m,l,x}_coco-ONNX`.
  Spike used `dfine_s` fp32 (41.5MB). Production default: `dfine_m`(52.3 mAP)/`dfine_l`(54.0) — same I/O.
- I/O: input `pixel_values[1,3,640,640]` f32 → `logits[1,300,80]` + `pred_boxes[1,300,4]` f32.
- Preprocess (RTDetrImageProcessor): resize **exactly** to 640×640 (squash aspect, bilinear), ×1/255,
  **no mean/std** (`do_normalize:false`). Loads fine at default optimization (`All`).
- Decode: per-query sigmoid over 80 classes → argmax; keep ≥ ~0.45; box is cxcywh normalized →
  xyxy × (origW, origH). DETR-style, NMS-free (light dedup optional for near-duplicates).
- Latency: ~108 ms/image (CoreML, 640²). Validated on COCO: cats.jpg→2 cats@96%, street.jpg→14
  people@94%; boxes correctly scaled. COCO classes → People / Animals / Vehicles buckets.

## Captioning (Florence-2-base-ft, MIT) — VIABLE ✅ (with two required workarounds)

- Components (`onnx-community/Florence-2-base-ft`, q4f16, ~280MB total + `tokenizer.json` 2.3MB):
  `vision_encoder`, `embed_tokens`, `encoder_model`, `decoder_model`, `decoder_with_past_model`.
- **GOTCHA 1 — must use `GraphOptimizationLevel::Level1`.** At default (`All`) every component fails
  to load (`SimplifiedLayerNormFusion` references a missing node). Level1 loads all of them.
- **GOTCHA 2 — must use the non-merged decoder pair.** `decoder_model_merged*` fails to load at ALL
  optimization levels (invalid If-subgraph: outer-scope value returned directly). Use
  `decoder_model.onnx` (first pass) + `decoder_with_past_model.onnx` (subsequent steps) instead.
- q4f16 graph **I/O is Float32** (weight-only quant) — no `half` feature / f16 tensor handling needed.
- Verified I/O contract (Level1):
  - vision_encoder: `pixel_values[*,3,*,*]` → `image_features[*,N,768]`
  - embed_tokens: `input_ids[*,*]` → `inputs_embeds[*,*,768]`
  - encoder_model: `attention_mask[*,*]`, `inputs_embeds[*,*,768]` → `last_hidden_state[*,*,768]`
  - decoder_model: `encoder_attention_mask`, `encoder_hidden_states[*,*,768]`, `inputs_embeds`
    → `logits[*,*,51289]` + `present.{0..5}.{decoder,encoder}.{key,value}[*,12,*,64]`
  - decoder_with_past: above + `past_key_values.{0..5}.{decoder,encoder}.{key,value}` → logits + present
- Generation = standard seq2seq greedy w/ KV cache: vision→embed(prompt)→concat→encoder→decode loop
  (decoder_model first, then decoder_with_past), BART `tokenizer.json` to decode ids. Phase-1 work.
- usls (0.2.0-alpha.3) **rejected**: alpha API, pins `ort =2.0.0-rc.11` (≠ our rc.12), 24 deps. Hand-roll.

## Decisions for implementation

- One runtime: `ort` + CoreML (no usls, no candle). Per-model optimization level (detector=All,
  Florence-2=Level1). Models downloaded first-run to app-data `models/` (D-FINE ~120MB + Florence-2
  q4f16 ~280MB + tokenizer). `tokenizers` crate (HF) for BART decode.
- Detector path is zero-risk; captioner is the one complex piece — validate end-to-end caption output
  early in Phase 1.
