# Evidence harnesses — which command produces which number

Astra cannot see the photographs. A packet whose `MEASURED EVIDENCE` block says "the greens look
off" is a wasted call; one that says "patch mean (0.412, 0.488, 0.331) vs golden (0.409, 0.451,
0.336) — the G channel is +8.2%" is worth several.

**Gate condition 2 lives here.** Before spending Astra quota, check whether one of these already
answers the question. If it does, run it instead.

## Decode and colour

| Question | Command |
|---|---|
| Decode stats — dims, WB multipliers, patch mean, highlight chroma | `cargo run --release -p core-raw --example corpus_probe [FILE..]` |
| Re-record the corpus goldens after an intended change | `cargo run --release -p core-raw --example corpus_probe --record` |
| Camera matrices as actually selected at a given CCT | `cargo run -p core-raw --example print_color_matrices` |
| Does rawler decode this body at all | `cargo run -p core-raw --example decode_gate` |
| Does libheif decode this `.HIF` | `cargo run -p core-raw --example heif_gate DIR` |
| PQ anchor ΔEV vs a same-capture CR3 | `cargo run -p core-raw --example calibrate_pq A.CR3 A.HIF` |
| Decode throughput | `cargo run --release -p core-raw --example bench_decode` |

## Develop and export

| Question | Command |
|---|---|
| Develop pixels for a parameter set (decode → GPU → PNG in `/tmp`) | `cargo run -p core-pipeline --example render_one` |
| Full-res export path | `cargo run -p core-pipeline --example export_full` |
| Colour-balance-RGB grading behaviour | `cargo run -p core-pipeline --example cb_demo` |
| Crop / straighten viewport behaviour | `cargo run -p core-pipeline --example crop_demo` |
| GPU render timing | `cargo run --release -p core-pipeline --example bench_render` |

## HDR and panorama

| Question | Command |
|---|---|
| Tripod bracket merge → EXR in `/tmp` | `cargo run -p core-hdr --example merge_one DIR` |
| Export a merged HDR as DNG | `cargo run -p core-raw --example export_hdr_dng` |
| Panorama feature detection over a directory | `cargo run -p core-pano --example detect_dir` |
| Full panorama stitch — inliers, residuals, focal estimates, convergence | `cargo run -p core-pano --example stitch_dir` |

## Analysis and library

| Question | Command |
|---|---|
| Single-image denoise | `cargo run --release -p core-analyze --example denoise_one` |
| Single-image analyze / detect / caption / faces | `cargo run --release -p core-analyze --example {analyze_one,detect_one,caption_one,faces_one}` |
| Detection eval against labels | `cargo run --release -p core-analyze --example detect_eval` |
| SAM segmentation gate | `cargo run --release -p core-analyze --example sam_gate` |
| Index the whole library + thumbs | `cargo run -p core-library --example scan_library` |
| Catalog query timing at scale | `cargo run --release -p core-library --example bench_catalog` |
| Import throughput | `cargo run --release -p core-import --example bench_import` |

## Goldens — what is already pinned

These are the falsification oracles. If one of them would move, say so in `VERSIONING IMPACT`.

| Golden | Covers |
|---|---|
| `cargo test -p core-pipeline --test param_effects` (807 lines) | 15 develop-parameter cases through the real GPU pipeline. Also guards the `wb_gain` no-padding invariant. |
| `cargo test -p core-pipeline --test masks` (531) | Mask stroke, feathering, refinement on GPU |
| `cargo test -p core-pipeline --test viewport` (407) | Crop / straighten / zoom transforms |
| `cargo test -p core-raw --test corpus` (569) | Multi-maker decode, 19 CC0 files × 2 tiers, against `tests/corpus/expected.toml` |
| `cargo test -p core-raw --test synthetic_bayer` (342) | Synthetic DNG generation + decode round-trip |
| `cargo test -p core-pano --test compositing` (456) | Blending, seam carving, exposure compensation |
| `cargo test -p core-pano --test streaming` (332) | Incremental merge |
| `cargo test -p core-dedup --test grouping` (313) | Byte-hash + same-capture grouping |

Corpus fixtures: `tests/corpus/manifest.toml` (19 CC0 samples, Tier 1 ≈367 MB / Tier 2 ≈678 MB),
expectations in `tests/corpus/expected.toml`, fetched by `scripts/fetch_raw_corpus.sh`.

Environment gates: `DARKROOM_REQUIRE_FIXTURES=1`, `DARKROOM_REQUIRE_CORPUS=1`,
`DARKROOM_CORPUS_TIER=2`, `DARKROOM_CORPUS_QUICK=1`, `DARKROOM_RAW_CORPUS=<dir>`.

## Reporting evidence into a packet

For every measurement, give: **the exact command**, the input file (corpus-relative or anonymised —
never a personal library path), and the raw output. Then state the delta against the reference or
golden, as a number. If there is no reference, say `EXPECTED / REFERENCE: unknown` — do not invent
one.

A `calibrate`-tagged constant coming back from Astra must name one of these harnesses and the number
to look for, or it is not a usable answer.
