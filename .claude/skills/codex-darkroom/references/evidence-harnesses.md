# Evidence harnesses — which command produces which number

Astra cannot see the photographs. A packet whose `MEASURED EVIDENCE` block says "the greens look
off" is a wasted call; one that says "patch mean (0.412, 0.488, 0.331) vs golden (0.409, 0.451,
0.336) — the G channel is +8.2%" is worth several.

**The gate's soft condition lives here.** Before packing, check whether one of these already answers
the question. If it does, run it instead. If it *could* answer it but only after hours of
re-recording or recalibration, say so in the packet and ask anyway — that is a disclosure, not a
veto.

## Decode and colour

| Question | Command |
|---|---|
| Decode stats — dims, WB multipliers, patch mean, highlight chroma (~12 recorded statistics per RAW) | `cargo run --release -p core-raw --example corpus_probe [FILE..]` |
| Golden-vs-actual table for the same statistics | `cargo run --release -p core-raw --example corpus_probe --diff` |
| Re-record the corpus goldens after an intended change | `cargo run --release -p core-raw --example corpus_probe --record` |
| Camera matrices as actually selected at a given CCT, plus the derived `PP_TO_SRGB` for the shader | `cargo run -p core-raw --example print_color_matrices` |
| Does rawler decode this body at all | `cargo run -p core-raw --example decode_gate` |
| Does libheif decode this `.HIF` — bit depth, chroma, nclx primaries/transfer/matrix | `cargo run -p core-raw --example heif_gate DIR` |
| PQ anchor ΔEV vs a same-capture CR3 (decision rule: keep 203 if \|ΔEV\| ≤ 0.25) | `cargo run -p core-raw --example calibrate_pq A.CR3 A.HIF` |
| Author a synthetic 10-bit PQ HEIF fixture (prints the exact code values written) | `cargo run -p core-raw --example gen_pq_fixture` |
| Decode throughput, full-res vs half-res superpixel | `cargo run --release -p core-raw --example bench_decode` |

## Develop and export

| Question | Command |
|---|---|
| Develop pixels for a parameter set (decode → GPU → PNG in `/tmp`) | `cargo run -p core-pipeline --example render_one` |
| **Where a correctly-exposed mid-grey lands** — prints `g0` and the `baseline_gain = 0.18/g0` it implies. The canonical `calibrate`-mode harness | `cargo run -p core-pipeline --example measure_midgrey` |
| Full-res export path | `cargo run -p core-pipeline --example export_full` |
| Colour-balance-RGB grading behaviour | `cargo run -p core-pipeline --example cb_demo` |
| Crop / straighten / autozoom viewport behaviour | `cargo run -p core-pipeline --example crop_demo` |
| GPU render timing (full-res, viewport, encode) | `cargo run --release -p core-pipeline --example bench_render` |

## HDR and panorama

| Question | Command |
|---|---|
| Bracket merge → EXR in `/tmp`; prints per-frame EV₁₀₀, relative scale, recovered alignment, output headroom | `cargo run -p core-hdr --example merge_one DIR` |
| The same merge **without** alignment/deghosting (tripod accumulator) — the A/B for a deghost question | `cargo run -p core-hdr --example merge_one DIR --no-align` |
| Export a merged HDR as float LinearRaw DNG | `cargo run -p core-raw --example export_hdr_dng` |
| Panorama feature detection + verified edges (confidence / overlap / shift / class) over a directory | `cargo run -p core-pano --example detect_dir` |
| Full panorama stitch — per-pair inliers, recovered focals, relative yaw, BA RMS | `cargo run -p core-pano --example stitch_dir` |
| Stitch straight from CR3s (progress + output dims) | `cargo run -p core-raw --example stitch_cr3` |

## Analysis, statistics and library

| Question | Command |
|---|---|
| Single-image denoise — mean \|Laplacian\| smoothness before/after through pack → tile → unpack → develop | `cargo run --release -p core-analyze --example denoise_one` |
| Single-image analyze / detect / caption / faces | `cargo run --release -p core-analyze --example {analyze_one,detect_one,caption_one,faces_one}` |
| Face embeddings — per-face box, score, L2 norm, and the pairwise cosine-distance matrix | `cargo run --release -p core-analyze --example faces_one` |
| Detection eval through the production decode path (the false-positive regression harness) | `cargo run --release -p core-analyze --example detect_eval` |
| Animal false-positive check (defaults to the 4 known FP frames; expect 0) | `cargo run --release -p core-analyze --example animal_eval` |
| Presence probe: per-image pre-gate scores (JSONL) + per-category metrics | `cargo run --release -p core-analyze --example presence_eval` |
| Presence probe: the **max-F1 operating point** — per-category thresholds + the global `VERIFY_ACCEPT` | `cargo run --release -p core-analyze --example presence_tune` |
| Refit the presence probe — weights as JSON, AUC and max-F1 to stderr | `cargo run --release -p core-analyze --example train_presence` |
| SAM segmentation gate — encode/decode latency + mask coverage | `cargo run --release -p core-analyze --example sam_gate` |
| ONNX input/output tensor signatures across graph-optimisation levels | `cargo run --release -p core-analyze --example onnx_io` |
| **`core-suggest` cross-validation report** — AUC / AUPRC / max-F1 / burst top-1 agreement against a real catalog, without writing the DB. The only harness this crate has | `cargo run --release -p core-library --example train_suggest` |
| Per-image `image_features` vector | `cargo run --release -p core-library --example features_one` |
| Panorama group detection over a real catalog (headless mirror of `pano_detect.rs`) | `cargo run --release -p core-library --example detect_catalog` |
| JSONL training rows from `user_events` + `image_features` | `cargo run --release -p core-library --example export_training_data` |
| Index the whole library + thumbs | `cargo run -p core-library --example scan_library` |
| Catalog query timing at 10k/50k/100k rows | `cargo run --release -p core-library --example bench_catalog` |
| Import throughput and peak RSS across worker counts | `cargo run --release -p core-import --example bench_import` |

## Goldens — what is already pinned

These are the falsification oracles. If one of them would move, say so in `VERSIONING IMPACT`.

| Golden | Covers |
|---|---|
| `cargo test -p core-pipeline --test param_effects` (807 lines) | 15 develop-parameter cases through the real GPU pipeline; the std140 layout regression guard, including `wb_gain` no-padding |
| `cargo test -p core-pipeline --test masks` (531) | Mask packing on CPU; stroke, feather and refinement on GPU |
| `cargo test -p core-pipeline --test viewport` (407) | Crop / straighten / zoom geometry — a zoomed sub-window must equal the same region of the whole-frame develop |
| `cargo test -p core-pipeline --test hsl` (120) | A red-band HSL move shifts red, leaves blue |
| `cargo test -p core-pipeline --test tone_curve` (114) | Identity curve is a no-op; a brightening curve lifts mid-grey |
| `cargo test -p core-raw --test corpus` (569) | Multi-maker decode, 19 CC0 files × 2 tiers, against `tests/corpus/expected.toml` |
| `cargo test -p core-raw --test synthetic_bayer` (342) | Fixture-free synthetic DNGs: neutral patch, blown highlight → neutral reconstruction, orientation, X-Trans, mono |
| `cargo test -p core-raw --test heif_decode` (189) | Committed synthetic PQ HEIF decodes to the expected linear values; orientation not double-applied |
| `cargo test -p core-raw --test hdr_exr` (128) | Merged-HDR EXR round-trip through fp16 storage, metadata survives |
| `cargo test -p core-raw --test decode_panic` (99) | A rawler or denoiser panic surfaces as typed `RawError::DecoderPanic` |
| `cargo test -p core-raw --test denoise_seam` (85) | An identity denoiser round-trips byte-identically through the write-back path |
| `cargo test -p core-raw --test decode_once` (64) | One decode yields sensor-native and EXIF-oriented views identical to the two legacy decoders |
| `cargo test -p core-raw --test preview_decode` (82) · `hdrpq_cr3` (90) · `library_sample` (63) | Half-res preview agreement; HDR-PQ CR3 preview fallback; real-CR3 end-to-end. Fixture-gated |
| `cargo test -p core-pano --test compositing` (456) | Warp → exposure gains → seam → multi-band blend → crop on a synthetic rotating-camera scene |
| `cargo test -p core-pano --test streaming` (332) | Streaming vs resident stitching agree on poses and canvas; cancellation |
| `cargo test -p core-pano --test align` (139) | `estimate_alignment_rgb` recovers a known warp sub-pixel; a blank input returns `None` |
| `cargo test -p core-pano --test detect` (139) · `end_to_end` (59) | Grouping accepts a rotating trio, rejects burst twins; `register()` recovers ground-truth yaw and focal |
| `cargo test -p core-dedup --test grouping` (313) | Byte-hash + same-capture-fingerprint grouping |
| `cargo test -p core-preset --test {preset,lr_import,robustness}` | Sparse merge, LR `.xmp` mapping fidelity, hostile-input guards |

Corpus fixtures: `tests/corpus/manifest.toml` (19 CC0 samples, Tier 1 ≈367 MB / Tier 2 ≈678 MB),
expectations in `tests/corpus/expected.toml`, fetched by `scripts/fetch_raw_corpus.sh`.

Environment gates: `DARKROOM_REQUIRE_FIXTURES=1`, `DARKROOM_REQUIRE_CORPUS=1`,
`DARKROOM_CORPUS_TIER=2`, `DARKROOM_CORPUS_QUICK=1`, `DARKROOM_RAW_CORPUS=<dir>`.

## What is NOT pinned — state this in every packet that touches it

The gap is the most valuable line in a packet. These areas have essentially no integration coverage:

- **`core-suggest`** — 1,834 lines of statistics with **zero `tests/` and zero `examples/` of its
  own**. 22 inline unit tests; the only end-to-end harness is `core-library`'s `train_suggest`.
- **`core-hdr`** — no `tests/` directory at all. 15 inline tests and `merge_one`. Nothing pins the
  deghost weighting or the aligned path against a reference.
- **`core-dedup`** — the hash and threshold math (DCT pHash, NCC, medoid clustering, the
  `accepted_pair` rule) has 2 inline tests plus one DB-level grouping test. Thresholds are unpinned.
- **`src-tauri/tests/` does not exist.** There is no crate-level test for the Tauri layer; E2E lives
  in the repo-root `e2e/` Playwright suite.

Inline `#[test]` counts carry most of the pure-math coverage: core-raw 50, core-pipeline 34,
core-pano 29, core-suggest 22, core-analyze 18, core-hdr 15, core-dedup 2, core-preset 0.

## Reporting evidence into a packet

For every measurement, give: **the exact command**, the input file (corpus-relative or anonymised —
never a personal library path), and the raw output. Then state the delta against the reference or
golden, as a number. If there is no reference, say `EXPECTED / REFERENCE: unknown` — do not invent
one.

Attached images (`IMAGES` block, `-i` on the command line) locate an artefact; they never carry the
measurement. `gpu-visual-qa` produces both.

A `calibrate`-tagged constant coming back from Astra must name one of these harnesses and the number
to look for, or it is not a usable answer.
