# Darkroom invariants — quotable, not paraphrasable

Every fact here was verified against the tree. **Quote these verbatim into a packet's `INVARIANTS`
block, with the `file:line`.** Never paraphrase from memory: a paraphrased invariant is how Astra
ends up deriving a correct answer to the wrong problem, and a *stale* one is how it derives a
correct answer to last month's problem.

Re-verify a citation before quoting it if the file has changed since this document was written
(2026-09-11, `PROCESS_VERSION` 5, rawler `0.8.0`).

---

## Working space

- The develop working space is **linear wide-gamut ProPhoto, scene-referred, D50**, with headroom
  above 1.0 preserved. ProPhoto→sRGB happens **in-shader at the display transition only**
  (`crates/core-pipeline/src/develop.wgsl`).
- `XYZ_TO_PROPHOTO_D50` — `crates/core-raw/src/color.rs:16`. D50 reference white at `color.rs:31`.
- `BT2020_TO_XYZ_D65` — `color.rs:38`; `bt2020_to_prophoto_d50()` at `color.rs:105` composes
  BT.2020→XYZ(D65) → Bradford CAT (`color.rs:90`) → XYZ→ProPhoto(D50). Its derivation is asserted by
  the unit test `bt2020_matrix_derivation` (`color.rs:429`) to 1e-6.
- The reverse direction for display-referred sources: `srgb_to_prophoto` (`core-raw/src/display.rs:94`)
  is computed as the exact `pseudo_inverse` of the shader's `pp_to_srgb`, so an unedited JPEG
  round-trips. sRGB EOTF/OETF at `display.rs:99`/`display.rs:108`.

**Every quantity in a packet must state its space, encoding (linear / log / OETF), white point and
units.** Most colour bugs in this repo are a quantity in the wrong space, not a wrong formula.

## RAW colour path — canonical order

`crates/core-raw/src/develop.rs::map_3ch_to_rgb` (`develop.rs:891`):

```
as-shot WB
  → clipped-highlight reconstruction (dcraw blend_highlights-style chroma shrink,
    raw channels ≥ HL_LO)
  → dual-illuminant camera matrix (color.rs::select_cam_matrix, DNG-spec 1/CCT interpolation)
  → row-normalised cam→ProPhoto
  → clip_negative
```

- `const HL_LO: f32 = 0.92;` — `develop.rs:797`; `reconstruct_clipped` — `develop.rs:854`. The dcraw
  `blend_highlights` basis (row 0 = luma channel sum, rows 1–2 = two chroma axes) is documented at
  `develop.rs:800` and `develop.rs:838`. It drives chroma to zero at unchanged luma, so a blown
  highlight develops neutral at `mean(wb_gains)` rather than carrying the as-shot cast.
- `select_cam_matrix` — `color.rs:339`; `neutral_cct` (CCT from the as-shot neutral) — `color.rs:271`;
  `interpolate_cam_matrix` (mired lerp between illuminants) — `color.rs:290`; `best_matrix_at` —
  `color.rs:323`.
- `guard_developable` (`develop.rs:472`) screens unsupported CFA layouts before any of this runs.
- **Any change to this path changes developed pixels ⇒ `PROCESS_VERSION` bump.**

## HDR PQ (HEIF `.hif`)

- `PQ_MAX_NITS = 10000.0` — `color.rs:135`. `pq_eotf()` — `color.rs:155`, returns a 0–1 fraction of
  `PQ_MAX_NITS`.
- `HDR_DIFFUSE_WHITE_NITS = 300.0` — `color.rs:150`. **Calibrated, not assumed**: the calibration
  record at `color.rs:142` states it started from BT.2408's 203-nit diffuse white and was measured
  against a real R7 CR3+HIF pair to ≈302 → 300. The caveat is recorded there too — that HIF is an
  in-camera HDR *composite*, so the anchor carries Canon's own HDR tone handling.
- Anchoring 300 nits to working-space 1.0 leaves ≈33× specular headroom (`core-raw/src/heif.rs`).
- Sanity test `pq_100_nits_spot_value` — `color.rs:403`.

## HDR merge

**This section was stale until 2026-09-11 — merge is no longer tripod-only.** Alignment and
deghosting both exist.

- `EV₁₀₀ = log₂(N²/t · 100/ISO)` — `crates/core-hdr/src/lib.rs:56` (`ev100`).
  **Higher EV₁₀₀ = less light captured = darker frame** — an easy sign error.
- Exposure ratio to the reference frame is `exp2(ev100(frame) - ev100(reference))` —
  `relative_scale`, `lib.rs:69`. Reference frame = median EV — `reference_index`, `lib.rs:75`.
- **Hat weighting** (`hat_weight`, `lib.rs:102`): high side full confidence below
  `W_HIGH_FULL = 0.75` fading to zero at `W_HIGH_ZERO = 0.9` (`lib.rs:92`–`:93`); low side ramps
  down below `W_LOW_KNEE = 0.10`, floored at `W_LOW_FLOOR = 0.05` (`lib.rs:96`–`:97`). **One weight
  per pixel, not per channel** — per-channel weighting fringes at the clip boundary.
- **Deghosting** (`DeghostParams`, `lib.rs:114`; applied in `accumulate`, `lib.rs:248`):
  `consist = exp(−(d/denom)²)` where `d` is the sum of absolute per-channel differences against the
  reference at reference-exposure scale, and `denom = sigma + k·max(reference)`. Defaults
  `sigma = 0.05`, `k = 0.25`, both in **linear radiance at the reference exposure, not display
  units**. The weight is scaled by `1 − ref_conf·(1 − consist)` (`lib.rs:276`), so where the
  reference clips (`ref_conf → 0`) deghosting **disables itself** and highlight recovery wins.
- **Alignment** is delegated to `core_pano::align::estimate_alignment_rgb` (`core-pano/src/align.rs:43`),
  whose default model is **affine, not projective** — the projective terms are wrong for a small
  hand-held delta (`align.rs:23`). The transform comes back row-major `[[f64;3];3]`.
- `core-hdr/src/warp.rs` applies it: a single f64 cofactor inversion (`invert3`, `warp.rs:96`,
  deliberately nalgebra-free), per-pixel back-projection and bilinear sampling (`warp.rs:72`), and a
  validity mask. Masked pixels are **skipped, not floored** (`add_frame_masked`, `lib.rs:229`):
  `hat_weight(0.0) == W_LOW_FLOOR` would otherwise pull out-of-frame warp borders in as visible
  halos (`lib.rs:225`).
- Output is **fp16** EXR, ZIP-compressed, linear ProPhoto scene-referred
  (`crates/core-raw/src/hdr_file.rs:4`). Samples are sanitised NaN→0, negatives floored, and clamped
  to `F16_MAX = 65504.0` (`hdr_file.rs:49`, `hdr_file.rs:91`). **State the fp16 dynamic-range limit
  in every HDR packet** — an algorithm that needs f32 headroom cannot ship into this container
  unchanged.

## GPU develop pipeline

- Shader is `crates/core-pipeline/src/develop.wgsl` (726 lines). Companion shaders:
  `mask_prepass.wgsl` (170), `mask_refine.wgsl` (85), `brush_bake.wgsl` (64). They are in `src/`,
  **not** a `shaders/` directory.
- **`vec3 wb_gain` must NOT be padded** (`crates/core-pipeline/src/params.rs:1371` ↔ the shader). A
  scalar packs into the vec3 tail per std140/WGSL; this is correct. **A past review false-flagged it**
  and the golden `core-pipeline/tests/param_effects.rs` guards it. Include this invariant in every
  GPU-math packet or Astra will re-flag it and waste a finding slot.
- Global white balance rides the `@binding(8)` CAT mat3. `ParamsUniform.wb_gain` is **held at
  identity** `[1.0, 1.0, 1.0]` (`params.rs:1145`); masks keep the per-channel gain delta via
  `wb_gain_from` (`params.rs:603`, comment at `params.rs:1143`).
- Bindings **0–15 are all in use; next free is `@binding(16)`**. Never alter `ParamsUniform` — new
  GPU data takes a new binding. Map: 0 `input_tex`, 1 `input_smp`, 2 `ParamsUniform` (guarded),
  3 tone-curve LUT, 4 HSL `FxUniform`, 5–7 masks, 8 white-balance CAT mat3, 9 `ExtraUniform`
  (Detail + vignette + Presence), 10 `ToneOpUniform`, 11 `base_lut`, 12 `GeomUniform`,
  13 `ViewUniform`, 14 `CbRgbUniform`, 15 `ChanMix`.
- `mask_refine.wgsl` is a separable **cross-bilateral** filter of mask alpha guided by display luma:
  17 taps per pass, spacing `max(sigma_px * 0.25, 0.5)` px so ±8 taps ≈ ±2σ and the feather scale is
  resolution-independent (`mask_refine.wgsl:64`); `sigma_px < 0.25` is a passthrough (`:60`).

## Tone operator and curves

- **Base tone operator** (`params.rs`): `BASE_LUT_SIZE = 512` (`params.rs:896`) samples over the
  log-exposure domain `BASE_U_MIN = -13.0` … `BASE_U_MAX = 8.0` (`params.rs:899`–`:900`), with
  `x = MID_GREY · 2^u` and `MID_GREY = 0.18` (`params.rs:902`). `build_base_curve_lut` —
  `params.rs:958`.
- `base_curve_value(x, amount)` (`params.rs:946`) interpolates between a **Reinhard neutral anchored
  at 0.18** (`f0 = x / (x + 0.82)`, so `f0(0.18) = 0.18`) at `amount = 0` and the **ACR fit** at
  `amount = 1` (mid-grey → ≈0.388).
- `acr_curve` (`params.rs:932`) is the reference table on `[0, TONE_X_JOIN]` with an asymptotic
  shoulder above: `TONE_X_JOIN = 0.875`, `TONE_Y_JOIN = 0.97702`,
  `TONE_SHOULDER_A = 0.2406 / (1 − TONE_Y_JOIN) ≈ 10.47`. Monotone, `f(0) = 0`, asymptotes to 1.0 —
  **it never hard-clips**.
- The reference table is `ADOBE_DEFAULT_TONE_CURVE` (`base_curve_ref.rs:19`), 1025 samples over
  scene-linear `x = i/1024`, taken from RawTherapee's `adobe_camera_raw_default_curve[1025]`
  (`rtengine/dcp.cc`, Beep6581/RawTherapee). Anchors: `f(0) = 0`, `f(1) = 1`, `f(0.18) ≈ 0.3874`
  (asserted at `base_curve_ref.rs:150`).
- **User tone curve** (`curve.rs`): monotone cubic Hermite (Fritsch–Carlson) — **no overshoot by
  construction** — sampled into a `LUT_SIZE = 256` entry RGBA8 LUT (`curve.rs:10`), master `rgb`
  composed first, then per-channel.
- **White balance** (`params.rs`): Planckian locus via the Kim et al. (2002) cubic `kim_xy`
  (`params.rs:678`), mired-symmetric `white_xy` (`:701`), `bradford_cat` (`:719`), `wb_matrix`
  (`:733`). Grading-RGB (Filmlight/Kirk, D65) matrices with a D50→D65 CAT at `:768`–`:776`.
- **Geometry** (`params.rs`): closed-form rotated-footprint containment `geom_autozoom` (`:1089`),
  `rot90_uv` (`:1103`), `geom_src_uv` (`:1115`), radial lens distortion `geom_lens_uv` with k1/k2
  (`:1132`).

## Versioning

- `PROCESS_VERSION` — `src-tauri/src/commands.rs:35`, currently **5**. Any change to the RAW colour
  path or shader math requires a bump; a bump invalidates every cached `<hash>_dev<PV>.jpg`. The v5
  rationale is recorded at `commands.rs:30`–`:34`.
- `DECODER_VERSION` — `crates/core-raw/src/lib.rs:49`, currently `"rawler-0.8.0"`. Recorded decode
  failures are skipped on rescan until this string changes.
- rawler is pinned `=0.8.0` (non-SemVer). All rawler calls stay inside `core-raw`, every public entry
  wrapped in `panic::catch_decode_panic`; the release profile must stay `panic = unwind`
  (`Cargo.toml:24`–`:27`).

## Panorama

`crates/core-pano` — the densest mathematics in the repo. Determinism is an invariant, not a nicety.

**Features and matching**: FAST-9 corners → intensity-centroid orientation → steered BRIEF-256
(`features.rs`, hand-rolled because imageproc's BRIEF pattern is not rotation-invariant), detected at
registration scale ≈900 px long side, keypoints stored in full-res pixels. Matching is brute-force
Hamming k=2 in **both** directions with `LOWE_RATIO = 0.8` (`matching.rs:15`) plus a mutual-best
cross-check — exact and deterministic, deliberately not LSH.

**Bundle adjustment** (`bundle.rs`): hand-rolled Levenberg–Marquardt over axis-angle rotation and
log-parameterised focal (reference rotation gauge-fixed), Huber-weighted ray-space residuals,
**numerical** Jacobian, solved via JᵀJ / Jᵀf with Cholesky and multiplicative damping.

| Constant | Value | Source |
|---|---|---|
| `HUBER_DELTA` | `0.01` | `bundle.rs:18` |
| `FD_EPS` (finite-difference step) | `1e-6`, scaled `FD_EPS * (1.0 + x[p].abs())` | `bundle.rs:20`, `bundle.rs:143` |
| `MAX_LM_ITERS` | `50` | `bundle.rs:21` |
| `COST_TOL` | `1e-8` | `bundle.rs:23` |
| initial λ | `1e-3`; ×10 on reject, ÷10 on accept, floor `1e-12`, bail above `1e12` | `bundle.rs:130`, `168`, `187`, `197` |
| Huber weight | `1` if `e ≤ δ`, else `sqrt(δ/e)` | `bundle.rs:239`–`242` |

**RANSAC** (`ransac.rs`): normalised (Hartley) 4-point DLT, symmetric transfer error, deterministic
sampling; plus a 3-point **affine** variant (`ransac_affine`/`affine_fit`, `ransac.rs:162`/`:226`)
used for the small-delta HDR case.

| Constant | Value | Source |
|---|---|---|
| `INLIER_THRESH_REG_PX` | `3.0`, in registration-scale pixels, converted by `/ reg_scale` | `ransac.rs:16`, `ransac.rs:94` |
| `MAX_ITERS` | `2000` | `ransac.rs:18` |
| `CONFIDENCE` | `0.99` | `ransac.rs:20` |
| overlap acceptance | `n_inliers ≥ 15 && n_inliers > 8.0 + 0.3·n_matches` — Brown & Lowe (2007) | `ransac.rs:41`–`43` |

**Determinism** (`rng.rs`): self-contained SplitMix64 (Vigna), deliberately **not** the `rand` crate,
so a transitive version bump cannot silently change every feature descriptor and RANSAC sample. Fixed
BRIEF test-pair pattern seed `0x5EED`; per-pair RANSAC seed `0xC0FFEE + pair_index` (`rng.rs:1`–`7`).
**Any proposal that introduces iteration-order dependence, an unstable sort, or float-accumulation
order sensitivity breaks this invariant.**

**Seam** (`seam.rs`): Voronoi init → monotone min-cost **DP seams** per verified pair
(`dp_seam_vertical` `seam.rs:317`, `dp_seam_horizontal` `:351`) over the COLOR_GRAD cost, with a
ghost penalty (`ghost_mask`, `:264`) routing seams around movers; a **graph-cut** path also exists
(`graph_cut`, `:514`).

| Constant | Value | Source |
|---|---|---|
| `GC_MAX_NODES` | `150_000` | `seam.rs:60` |
| `GC_COST_SCALE` | `256.0` | `seam.rs:63` |
| `GHOST_DIFF_MULT` | `8.0` | `seam.rs:67` |
| `GHOST_ABS_FLOOR` | `0.05` | `seam.rs:70` |
| `GHOST_DILATE` | `3` | `seam.rs:73` |
| `GHOST_PENALTY` | `1.0e4` | `seam.rs:76` |
| `WEIGHT_FLOOR` | `0.05` | `seam.rs:109` |
| `OUT_OF_BAND_COST` | `1.0e6` | `seam.rs:112` |

**Blend** (`blend.rs`): Burt & Adelson multi-band Laplacian splining, streaming. `BANDS = 5`
(`blend.rs:30`), `EPS = 1e-6` (`blend.rs:32`), pyramid kernel `[1, 4, 6, 4, 1]` (`blend.rs:276`).
Sub-rect origins are aligned down to a `2^(bands−1)` lattice so every frame shares the band lattice
exactly.

**Exposure compensation** (`exposure.rs`): Brown & Lowe gain solve, regularised least squares via
Cholesky. `WEIGHT_FLOOR = 0.05` (`:22`), `LAMBDA = 0.01` (`:24`), gain clamped to `[0.5, 2.0]`
(`:26`–`:27`).

**Projection** (`project.rs`): matched `project_ray`/`unproject` pairs per surface,
`BORDER_SAMPLES_PER_EDGE = 64` (`:19`).

**Rectangling** (`rectangle.rs`): simplified He, Chang & Sun (2013). Two quadratic energies solved by
conjugate gradient on the normal equations (`solve_axis`, `:256`), with a no-fold Jacobian guard
(`nofold_factor` `:422`, `min_norm_jacobian` `:442`).

| Constant | Value | Source |
|---|---|---|
| `TARGET_COLS` / `TARGET_ROWS` / `MIN_CELL` | `40` / `25` / `16` | `rectangle.rs:46`–`:49` |
| `BOUNDARY_WEIGHT` | `6.0` | `rectangle.rs:51` |
| `CG_MAX_ITERS` / `CG_TOL` | `500` / `1e-6` | `rectangle.rs:70`–`:71` |
| `JAC_MIN` | `0.05` | `rectangle.rs:75` |

## Denoise DSP

`crates/core-analyze/src/denoise.rs`.

- `pack` (`:76`) splits the 2×2 Bayer mosaic into 4 half-res planes permuted to canonical
  **`[R, Gr, Gb, B]`** for any RGGB/BGGR/GRBG/GBRG phase, black-subtracted and normalised by the
  per-position white level. `unpack` (`:116`) inverts it back to u16.
- `tile_and_blend` (`:182`) is fixed-size overlap-tiled inference with **reflect padding**
  (`reflect`, `:137`), `tile_origins` (`:155`), and linear-feather blending across the overlap.
  `PMRID_TILE = 256` (`:350`), `PMRID_INP_SCALE = 256.0` (`:351`).
- `bilateral_tile` (`:251`) is the model-free reference denoiser — spatial σ_s × range σ_r.
- **K-Sigma** (`KSigma`, `:316`; `convert`, `:331`) is the variance-stabilising ISO→(k, σ) mapping
  PMRID expects. `PMRID_KSIGMA` (`:343`): `k_coeff = [0.0005995267, 0.00868861]`,
  `b_coeff = [7.11772e-7, 6.514934e-4, 0.11492713]`, `anchor = 1600.0`, `v = 959.0`. **These are the
  upstream PMRID values, calibrated on a different sensor than the R7** — treat them as
  `literature<PMRID>`, not as calibrated-for-this-camera.
- The denoised u16 planes are written back into the mosaic and re-developed; with an identity
  denoiser that round-trip is byte-identical (`core-raw/tests/denoise_seam.rs`).

## Statistical modelling — `core-suggest`

- Feature layout: `EMB_DIM = 512`, `HAND_DIM = 16`, `DIM = 528`, `FEATURE_VERSION = 1`
  (`features.rs:13`–`:22`). **Missing values are `f32::NAN`, never 0.0** (`features.rs:6`) — 0.0 is a
  legal value for these signals; NaN is replaced at fit/score time by the weighted training-set
  column mean.
- `fit.rs`: class-balanced logistic regression (BCE) plus a within-burst **Bradley–Terry pairwise
  ranking** term, each normalised by its own weight mass so `pair_alpha` is a real mix knob
  (`fit.rs:5`). Batch gradient descent on **standardised** columns; the standardisation is folded
  back into the weights (`fold` `:167`, `unfold` `:178`) so inference is a plain `sigmoid(w·x + b)`.
  f64 accumulators, f32 storage.
- `weights.rs`: provenance trust × inverse-frequency class balance, `cw_pos = n/(2·n_pos)` computed
  over **non-`Batch` rows only** (`:26`), then a hard **`AGREE_MASS_CAP = 0.30`** (`:13`, applied at
  `:42`) so the model cannot bootstrap on its own confirmations. `derive_pairs` (`:69`) builds the
  within-burst (pick, reject) pairs.
- `cv.rs`: **group-aware** k-fold — folds are assigned by burst group so siblings never straddle
  train/test (`assign_folds` `:63`), and pairs are re-derived inside each training split to stop
  leakage through the ranking term. Out-of-fold scores only (`oof_scores`, `:82`).
  `DEFAULT_LAMBDAS = [1e-3, 3e-3, 1e-2, 3e-2, 1e-1]` (`:21`), `REJECT_MIN_PRECISION = 0.95` (`:25`),
  `TAU_UNREACHABLE = 2.0` (`:29`).
- `metrics.rs`: ROC-AUC via the **tie-aware Mann–Whitney U** mean-rank form (`:18`), AUPRC (`:58`),
  `max_f1` (`:111`), `precision_threshold` (`:129`), `burst_top1_agreement` (`:157`). Every undefined
  ratio returns `None` — **never NaN, never a silent 0.0**.
- `model.rs`: `MODEL_SCHEMA = 1` (`:14`), `MIN_PER_CLASS = 10` (`:17`).

## Similarity metric — `core-dedup`

All of it is `crates/core-dedup/src/lib.rs`; there are no separate hash or threshold files.

- `SIMILARITY_FEATURE_VERSION = 1` (`:13`).
- Hashes: 64-bit dHash (`dhash_from_image`, `:131`), DCT **pHash** with a median-of-coefficients
  threshold (`phash_from_coeffs`, `:193`; `median_f32`, `:215`), `hamming` (`:150`), 4×4 colour
  signature (`color_grid4x4`, `:220`).
- Distances: `color_distance` (`:597`), `chroma_distance` (`:605`), normalised cross-correlation
  `ncc_u8` (`:614`) / `ncc_f32` (`:636`), `edge_ncc` over Sobel-style `edge_magnitudes` (`:620`/`:624`).
- **The accept rule is a time-windowed multi-signal predicate**, not a single threshold
  (`accepted_pair`, `:473`): `strong_hash = (dh ≤ 4 && ph ≤ 10) || ph ≤ 8`,
  `medium_hash = dh ≤ 8 || ph ≤ 14`, `loose_hash = dh ≤ 14 || ph ≤ 22`,
  `structure = luma ≥ 0.72 || edge ≥ 0.40`, `strong_structure = luma ≥ 0.86 || edge ≥ 0.55`, combined
  differently for windows `0..=3 s`, `4..=30 s`, and beyond. `pair_score` (`:492`),
  `time_window_secs` (`:559`), `candidate_limits` (`:567`), and the `camera_conflict` (`:579`) /
  `aspect_conflict` (`:587`) guards.
- Grouping: `find_similarity_edges` (`:387`) → medoid clustering (`medoid_groups` `:499`,
  `next_center` / `center_members` through `:547`).

## Library-side numerics

- **Face clustering** (`crates/core-library/src/face_cluster.rs`): the Immich pattern — each
  unassigned face brute-force-searches its nearest neighbours by **cosine distance on L2-normalised
  ArcFace embeddings**, so distance = `1 − dot` (`cosine_dist`, `:53`). Same person ≈ 0.2, different
  person ≈ 0.8 (`:18`); `max_distance` defaults to **0.45**, overridable by
  `DARKROOM_FACE_MAX_DIST` (`:33`). The threshold is validated, not assumed — see `:134`. Vectors
  must be **truncated, never zero-padded**, when a dimension mismatch appears: padding shrinks cosine
  distance and causes false merges (`:139`). `CANCEL_CHECK_EVERY = 256` (`:51`).
- **Per-image features** (`crates/core-library/src/features.rs`, `compute_features` `:34`): computed
  from the linear develop at a normalised `FEATURE_EDGE = 512` long side so sharpness and histograms
  are comparable across images (`:11`). `LUMA_BINS = 256`, `CHROMA_BINS = 32` (a 32×32 log-chroma
  histogram over `log(r/g)`, `log(b/g)` clamped to `CHROMA_RANGE = 2.0`), `CLIP_HI = 0.99`,
  `CLIP_LO = 0.005` (`:12`–`:16`). These feed `core-suggest`'s 16 hand features, so changing one
  changes the model's input distribution — `FEATURE_VERSION` exists for that reason.

## Colour management — not yet implemented

Display P3 / AdobeRGB / ICC output transforms, gamut mapping and 3D LUT interpolation are **in scope
but not in the tree**. There is nothing to quote. A packet here is a `derive` or `explore` over the
existing working space and the `@binding(16)` constraint, and must say plainly that no implementation
exists rather than implying one does.

## Formats

`core_library::SUPPORTED_EXT` is the only gate: `cr3 cr2 crw nef nrw arw sr2 srf dng` + `jpg png hif
exr`. Unsupported bodies/modes (Nikon HE/HE*, Sony ARW6, bodies missing from rawler's DB, X-Trans)
surface as typed `RawError::Unsupported`, are recorded in `decode_failure`, and are skipped on
rescans until `DECODER_VERSION` changes.

## Constant provenance — the hard rule

Every constant above is either quoted from source or carries a recorded calibration. When a packet
needs a constant that is **not** in this file and **not** quotable from source with a `file:line`,
tag it `unknown` — never guess a plausible value. And when Astra returns a constant, it must carry
`derived` (with the derivation), `literature` (with a citation), or `calibrate` (with an experiment
recipe naming a harness from `evidence-harnesses.md`). **An unsourced numeric constant is a defect,
not an answer.**
