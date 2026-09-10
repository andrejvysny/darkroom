# Darkroom invariants — quotable, not paraphrasable

Every fact here was verified against the tree. **Quote these verbatim into a packet's `INVARIANTS`
block, with the `file:line`.** Never paraphrase from memory: a paraphrased invariant is how Astra
ends up deriving a correct answer to the wrong problem.

Re-verify a citation before quoting it if the file has changed since this document was written
(2026-09-10, `PROCESS_VERSION` 5, rawler `0.8.0`).

---

## Working space

- The develop working space is **linear wide-gamut ProPhoto, scene-referred, D50**, with headroom
  above 1.0 preserved. ProPhoto→sRGB happens **in-shader at the display transition only**
  (`crates/core-pipeline/src/develop.wgsl`).
- `XYZ_TO_PROPHOTO_D50` — `crates/core-raw/src/color.rs:16`. D50 reference white at `color.rs:31`.
- `BT2020_TO_XYZ_D65` — `color.rs:38`; `bt2020_to_prophoto_d50()` at `color.rs:105` composes
  BT.2020→XYZ(D65) → Bradford CAT → XYZ→ProPhoto(D50). Its derivation is asserted by the unit test
  `bt2020_matrix_derivation` (`color.rs:429`) to 1e-6.

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

- `const HL_LO: f32 = 0.92;` — `develop.rs:797`. The dcraw `blend_highlights` basis (row 0 = luma
  channel sum, rows 1–2 = two chroma axes) is documented at `develop.rs:800` and `develop.rs:838`.
- `select_cam_matrix` — `color.rs:339`.
- **Any change to this path changes developed pixels ⇒ `PROCESS_VERSION` bump.**

## HDR PQ (HEIF `.hif`)

- `PQ_MAX_NITS = 10000.0` — `color.rs:135`. `pq_eotf()` — `color.rs:155`, returns a 0–1 fraction of
  `PQ_MAX_NITS`.
- `HDR_DIFFUSE_WHITE_NITS = 300.0` — `color.rs:150`. **Calibrated, not assumed**: the calibration
  record at `color.rs:142` states it started from BT.2408's 203-nit diffuse white and was measured
  against a real R7 CR3+HIF pair to ≈302 → 300. The caveat is recorded there too — that HIF is an
  in-camera HDR *composite*, so the anchor carries Canon's own HDR tone handling.
- Sanity test `pq_100_nits_spot_value` — `color.rs:403`.

## HDR merge

- `EV₁₀₀ = log₂(N²/t · 100/ISO)` — `crates/core-hdr/src/lib.rs:56` (`ev100`), formula at `lib.rs:63`.
  **Higher EV₁₀₀ = less light captured = darker frame** (`lib.rs:55`) — an easy sign error.
- Exposure ratio to the reference frame is `exp2(ev100(frame) - ev100(reference))` — `lib.rs:70`.
- **Tripod v1 — no alignment, no deghosting.** `warp.rs` exists but merge does not align.
- Output is **fp16** EXR, ZIP-compressed, linear ProPhoto scene-referred
  (`crates/core-raw/src/hdr_file.rs:4`). Samples are sanitised NaN→0, negatives floored, and clamped
  to `F16_MAX` (`hdr_file.rs:48`, `hdr_file.rs:88`). **State the fp16 dynamic-range limit in every
  HDR packet** — an algorithm that needs f32 headroom cannot ship into this container unchanged.

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

## Versioning

- `PROCESS_VERSION` — `src-tauri/src/commands.rs:35`, currently **5**. Any change to the RAW colour
  path or shader math requires a bump; a bump invalidates every cached `<hash>_dev<PV>.jpg`.
- `DECODER_VERSION` — `crates/core-raw/src/lib.rs:49`, currently `"rawler-0.8.0"`. Recorded decode
  failures are skipped on rescan until this string changes.
- rawler is pinned `=0.8.0` (non-SemVer). All rawler calls stay inside `core-raw`, every public entry
  wrapped in `panic::catch_decode_panic`; the release profile must stay `panic = unwind`.

## Panorama

`crates/core-pano` — the densest mathematics in the repo. Determinism is an invariant, not a nicety.

**Bundle adjustment** (`bundle.rs`): hand-rolled Levenberg–Marquardt over axis-angle rotation and
log-parameterised focal, Huber-weighted ray-space residuals, **numerical** Jacobian, solved via
JᵀJ / Jᵀf with Cholesky and multiplicative damping.

| Constant | Value | Source |
|---|---|---|
| `HUBER_DELTA` | `0.01` | `bundle.rs:18` |
| `FD_EPS` (finite-difference step) | `1e-6`, scaled `FD_EPS * (1.0 + x[p].abs())` | `bundle.rs:20`, `bundle.rs:143` |
| `MAX_LM_ITERS` | `50` | `bundle.rs:21` |
| `COST_TOL` | `1e-8` | `bundle.rs:23` |
| initial λ | `1e-3`; ×10 on reject, ÷10 on accept, floor `1e-12`, bail above `1e12` | `bundle.rs:130`, `168`, `187`, `197` |
| Huber weight | `1` if `e ≤ δ`, else `sqrt(δ/e)` | `bundle.rs:239`–`242` |

**RANSAC** (`ransac.rs`): normalised 4-point DLT, symmetric transfer error, deterministic sampling.

| Constant | Value | Source |
|---|---|---|
| `INLIER_THRESH_REG_PX` | `3.0`, in registration-scale pixels, converted by `/ reg_scale` | `ransac.rs:16`, `ransac.rs:94` |
| `MAX_ITERS` | `2000` | `ransac.rs:18` |
| `CONFIDENCE` | `0.99` | `ransac.rs:20` |
| overlap acceptance | `n_inliers ≥ 15 && n_inliers > 8.0 + 0.3·n_matches` — Brown & Lowe (2007) | `ransac.rs:41`–`43` |

**Determinism** (`rng.rs`): self-contained SplitMix64, deliberately **not** the `rand` crate, so a
transitive version bump cannot silently change every feature descriptor and RANSAC sample. Fixed
BRIEF test-pair pattern seed `0x5EED`; per-pair RANSAC seed `0xC0FFEE + pair_index` (`rng.rs:1`–`7`).
**Any proposal that introduces iteration-order dependence, an unstable sort, or float-accumulation
order sensitivity breaks this invariant.**

**Seam** (`seam.rs`): graph cut with ghost penalties.

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

**Blend** (`blend.rs`): `BANDS = 5` (`blend.rs:30`), `EPS = 1e-6` (`blend.rs:32`), pyramid kernel
`[1, 4, 6, 4, 1]` (`blend.rs:276`).

**Exposure compensation** (`exposure.rs`): `WEIGHT_FLOOR = 0.05` (`:22`), `LAMBDA = 0.01` (`:24`),
gain clamped to `[0.5, 2.0]` (`:26`–`:27`).

**Projection** (`project.rs`): `BORDER_SAMPLES_PER_EDGE = 64` (`:19`).

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
