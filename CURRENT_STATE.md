# Darkroom — Current State (handoff)

> Snapshot for resuming in a new session. Pairs with `TODO.md` (what's next + leftovers), `README.md`
> (overview), `SPEC_V1.md` (full spec).

> **2026-07-20 — HDR/Pano review pass (UNCOMMITTED on `main` `4a0bc57`).** Senior review of the
> three landed subsystems (HDR · panorama merge · panorama detection) + remaining-work execution.
> Plan: `~/.claude/plans/act-as-senior-software-elegant-flurry.md`; live status in `TODO.md` top
> section. Landed so far: **Track A** (8 verified correctness bugs — detect-merge misattribution,
> dishonest `panorama_sources` links, `hdr_merge` mixed-camera/duplicate-id holes, orphan/truncated
> output files on failure, merge-gated-on-stale-preview), **Track B** (hand-held HDR: affine
> alignment + reference-based deghosting + cancel; `hdr_sources` + Source-frames UI; float-DNG
> export; macOS libheif dylib bundling + CI installs), **Track C** (panorama `FrameSource`
> streaming — peak frame RAM ~3.8 GB → ~0.55 GB at 10×32 MP — plus real cancel threading and
> `panorama_status` reconnect), **Track D** (detection state lifted to the store with singleton
> listeners, zombie-suggestion pruning, LeftNav Detect button). **All four tracks landed.**
> `cargo test --workspace` = 52 suites, 0 failures; clippy `--examples`, tsc, and build clean.
> Remaining work is the QA that needs the dev Mac + real captures (see `TODO.md`).
>
> **2026-07-20 (later) — validated against a real 2137-file / 38 GB R7 card dump.** Two hard bugs
> found and fixed: **every real `.HIF` was rejected** (Canon's primary item is a 4×5 grid, so
> libheif's handle-level nclx reads `Unspecified` — we now parse the container's `colr` boxes
> ourselves, `heif.rs::container_nclx`), and **557 HDR-PQ CR3s failed to index** (rawler can't
> extract their HEVC embedded preview; `thumb.rs` now falls back to developing the mosaic). Full
> index is 2134/2134, 0 failed. Also fixed a recall regression in the new pano streaming path
> (Triangle→Lanczos3 for the low-res pass: 8→12 frames registered on a real sweep). Risk R4
> (portrait HIF) is resolved — libheif applies `irot`, so `.oriented()` must NOT be added.
> Details + open items: `TODO.md` "Real-corpus validation round".
>
> **2026-07-25 — RAW+JPEG pairing (UNCOMMITTED on `main`).** The Import dialog now asks, whenever
> the source holds RAW+JPEG/HEIF shots, whether to **pair** them (companion linked to its RAW: one
> grid cell, both files catalogued and developable) or import them **standalone** (previous
> behaviour). Detection is path-only (same folder + case-insensitive stem, ≥1 RAW + ≥1 `.jpg`/
> `.jpeg`/`.hif`; PNG/EXR excluded as app products), so listing a card stays instant. Schema **21**
> (`021_image_pairs.sql`). Companions are hidden from the default grid + nav counts, excluded from
> every dedup detector (a RAW and its JPEG share a capture fingerprint and would otherwise read as
> duplicates), badged `+JPG` on the primary tile, listed with an Unpair action in the metadata panel,
> and revealed by LeftNav → "Show paired JPEGs". Pairing is **import-time only** — the FS watcher and
> `library_index_root` still catalogue companions standalone. Gates: `cargo test --workspace` green,
> clippy no new warnings, `npm run build` clean. **Live GUI QA pending.**
>
> **Schema is now 20** (`020_hdr_sources.sql`). **New IPC**: `hdr_cancel`, `image_sources`,
> `hdr_export_dng`. **New harnesses**: `core-raw --example export_hdr_dng`,
> `core-library --example {detect_catalog,index_root}`.
> **macOS builds now require `brew install libheif`** — `bundle.macOS.frameworks` +
> `beforeBundleCommand` live in `src-tauri/tauri.macos.conf.json` (a macOS-only config overlay, so
> Windows/Linux `cargo check` isn't gated on staged dylibs), and `tauri-build` validates those
> paths at compile time → run `bash scripts/macos-bundle-dylibs.sh stage` once before building
> `src-tauri` on a fresh clone (CI does this automatically).

> **2026-07-18 — HDR pass (branch `claude/hdr-heif-support-5r5ztx`): Canon HDR PQ HEIF (`.hif`)
> full develop support + Merge-to-HDR (tripod v1, fp16 linear-ProPhoto EXR). See "Latest pass — HDR"
> below. Binding-map correction: bindings 0–15 are ALL used (15 = `ChanMix`); next free = 16 — any
> older "next free = 15" note below is stale.**

> **2026-07-02 — READ `TODO.md` top "Repo state sync (2026-07-02)" FIRST.** `main` is now `8c1072c`
> (0.2.0 released; presets-history / windows-hardening / jpeg-png all merged; render overhaul landed).
> Branch `feat/lens-corrections` (in progress) adds lens distortion/CA + pre-release signing scaffolding.
> Everything below dated 2026-06-26 predates those merges.

## Panorama detection (branch `claude/panorama-detection-ob1jjq`, 2026-07-19) — CURRENT

**"Detect panoramas"**: one click scans the whole library incrementally in the background, suggests
stitchable groups, and a review panel hands each group into the existing merge flow. Method = Brown &
Lowe "Recognising Panoramas" (the OpenCV-stitcher lineage) composed with an EXIF prefilter; no OSS
photo manager ships this, so there was no reference implementation.

- **Pipeline**: `core_library::pano_detect` metadata prefilter (present images with `capture_date`,
  ordered by camera key + time; clusters split on >30 s gap / camera / EXIF-orientation / focal-ratio
  >1.06 — NULLs compatible; oversize runs largest-gap-chunked to ≤48) → `core_pano::detect_groups`
  over cached **512 px thumbs** (always present after import, already EXIF-upright → homographies
  safe): the existing FAST+BRIEF / ratio+cross-check / RANSAC front end **unchanged**; per verified
  pair `conf = inliers/(8+0.3·matches)` (>1 by the existing gate, so overlap+shift is the real
  classifier), `overlap` = symmetric warped-quad∩rect area (Sutherland–Hodgman + shoelace), `shift`
  = ‖H·cᵢ−cⱼ‖/diagⱼ; **Pano** edge = overlap∈[0.10,0.92] ∧ shift≥0.10, **Burst** = >0.92 ∧ <0.05
  (excluded — also absorbs HDR brackets); union-find keeps **all** components ≥2 (`graph.rs` keeps
  only the largest — deliberately not reused); group confidence = min edge conf on the max spanning
  tree (weakest necessary link). Thresholds live in `DetectOptions` / `ClusterParams`.
- **Persistence** (migration **019**, schema = 19): `pano_detect_groups` upserted by `member_key`
  (blake3 of sorted member content hashes → dismissed/merged status survives rescans AND re-imports),
  `pano_detect_members` (`position` = capture order), `pano_detect_scan` (per-image `ALGO_VERSION`
  markers → incremental scans; bump `panodetect-v1` in `core-library/src/pano_detect.rs` to force a
  rescan; `force` bypasses markers but keeps dismissals via member_key).
- **Job** `src-tauri/pano_detect.rs` (panorama.rs RunGuard + brief-DB-lock discipline — thumb decode
  and detect fully unlocked): events `pano_detect:progress {phase:"cluster"|"verify",done,total}` /
  `pano_detect:done {found}` / `pano_detect:error {message}`; cancel drains to `done` with the
  partial count (analysis.rs convention). Unconditional module (no ML → no cfg stub).
- **Frontend**: `usePanoDetect` hook (per-instance StrictMode-safe listeners), `PanoSuggestions`
  overlay (thumb strips, confidence badge, capture range, dismiss/undo + show-dismissed; "Preview &
  Merge…" gated on `allRaw` — merge is RAW-only — hands member ids to `PanoramaModal` via
  `panoramaSources`), LeftNav "Panoramas" section + suggested count, CommandPalette entry;
  `panorama:done` records suggestion-originated merges via `pano_detect_mark_merged`
  (`detectGroupId` rides the `panorama_merge` IPC call and is echoed back on `panorama:done` —
  no ambient store field).
- **Validated on real photos** (this container): 19-image corpus (opencv_extra boat×6 /
  newspaper×4 / s×2 / a×3 + synthetic burst trio + unrelated control) → exactly the 4 ground-truth
  groups, bursts classified Burst and excluded, unrelated matched nothing, **0 false pairs of 171**
  at 512 px; every detected group then stitched clean via the harness. Mocked-Chromium UI
  walkthrough green (zero console errors). Harness:
  `cargo run --release -p core-pano --example detect_dir -- <dir> [--edge N] [--all-pairs] [--stitch]`.
- Also landed: merge-dialog preview cache now released on plain modal close
  (`panorama_preview_release`, closes TODO leftover #5a).
- **NOT yet done**: in-app run over the dev machine's real CR3 library (container has 1 CR3);
  threshold audit on messier corpora (hand-held multi-row, moving subjects); Tier-3 e2e of
  detect→review→merge.

## Panorama merge (branch `claude/darkroom-panorama-research-ruvw38`, 2026-07-18) — CURRENT

Lightroom-style **Photo Merge → Panorama** landed end-to-end (P0–P4 of the plan in
`~/.claude/plans/act-as-senior-rust-fuzzy-meadow.md`): select 2–10 raws → merge dialog (projection
Auto/Spherical/Cylindrical/Perspective, Boundary Warp slider [UI-only, inert v1], Auto Crop, live
low-res preview) → background full-res stitch → **16-bit LinearRaw DNG** written next to the first
source, registered in the catalog (linked via new `panorama_sources` table, migration **018**,
schema = 18) and editable in Develop **with full raw WB latitude** (no tone curve baked).

- **`crates/core-pano`** (NEW, pure Rust, never links rawler): Brown & Lowe registration (FAST +
  steered BRIEF-256, Hamming kNN2+ratio, Hartley-DLT RANSAC + `n>8+0.3m` verification, union-find
  graph, OpenCV-ported `focalsFromHomography` + median, spanning-tree rotation seed, hand-rolled LM
  ray-error BA gauge-fixed on the reference, `waveCorrect` port) + compositing (median-focal canvas,
  rayon inverse warp, Brown & Lowe per-channel gain LSQ, Voronoi+DP seams @~1400px, streaming 5-band
  multiband blend on a 16px-aligned lattice, largest-inscribed-rect autocrop, `max_long_side` cap).
  **Convention: rotations are world→camera** (ray = `Rᵀ·K⁻¹·x`) — OpenCV ports were re-derived for
  this (verbatim ports are camera→world and give wrong angles). All deterministic (internal
  SplitMix64, zero `rand` in the lib). Synthetic GT: angles ±0.15°, focal ±0.7%, seam gradient 0.10
  vs 0.89 threshold.
- **`core-raw::pano`**: `develop_camera_native` (demosaic + rescale + upright, NO wb/matrix — the
  stitch space) and `write_pano_dng` (rawler `DngWriter`, LinearRaw cpp=3, black 0 / white 65535,
  ColorMatrix+AsShotNeutral from the reference frame, embedded sRGB preview+thumb, EXIF
  pass-through, Orientation forced 1). **Round-trip test proves the DNG re-develops through the
  headroom-preserving ProPhoto branch** (`develop_linear_from`'s matrix path), NOT the calibrated
  fallback — write refuses loudly if the matrix map is empty. Shared front half extracted as
  `demosaic_camera_native` (no behavior change to normal develop).
- **`src-tauri/panorama.rs`** + commands `panorama_preview/merge/cancel/status` (denoise.rs shape:
  atomics + Drop guard, `spawn_blocking`, `panorama:progress {phase}` / `done {imageId}` / `error`).
  Preview caches downscaled (1400px) frames per id-list so projection toggles restitch instantly.
  **DB lock discipline (ea0d66a): decode/stitch/encode fully unlocked; one brief tx for
  `insert_image` + link rows.** Composite capped at `min(12000, gpu max_texture_dim)` so the pano
  opens in the (tiling-free) develop pipeline. Same-camera enforced (one ColorMatrix per DNG).
- **Frontend**: `PanoramaModal` (ExportModal pattern; `panoramaSources` store field), `PanoramaPill`
  (DownloadPill pattern), SelectionBar entry (2–10), `usePanorama` hook, ipc.ts wrappers.
- **Validated headless**: `cargo test` green across core-db (6) / core-raw (8+4) / core-pano (17);
  clippy clean; `npm run build` clean. **NOT yet done: real multi-frame visual QA** (container has
  only 1 CR3) — on the dev machine run
  `cargo run --release -p core-raw --example stitch_cr3 -- <dir-of-pano-CR3s> /tmp/o.dng` and open
  the DNG in Develop (WB slider must behave like a raw; optionally Adobe `dng_validate`).
- **v1 limits (see TODO)**: `merge` holds all full-res sources in RAM (~0.4 GB/frame → ~4 GB at 10
  frames); ~~preview cache freed on merge but not on plain modal close~~ (fixed 2026-07-19:
  `panorama_preview_release`). Boundary Warp, deghosting, and graph-cut seams are NOT limits
  anymore: Boundary Warp is wired end-to-end (`core-pano/src/rectangle.rs`, `b7fdfc3`), deghosting
  is gain-corrected diff + median ghost mask as a hard seam penalty, and graph-cut seams are
  implemented but DP stays the default (benchmark: ~35 ms DP vs ~42 s graph-cut at identical seam
  quality — see TODO).

## Repo state sync (2026-06-26)

`main` = **`f7445df`**, `origin/main` = **`1cbb3e3` (v0.1.1)**, **2 unpushed** (`e880fda` GPU hardening,
`f7445df` progressive preview). Merged to `main` since the prose below was last accurate (and not yet
folded into it): `feat/windows-packaging` (NSIS + DirectML + Windows CI), **dedup redesign** (UI +
similarity-pipeline tightening), **diagnostic logging**, Intel-macOS/beta CI, the **v0.1.1 release**,
and the GPU/preview fixes. `feat/presets-history` is **100% uncommitted** on `f7445df`; its hardening
pass is DONE + headless-green (2026-06-26) — only in-app QA + commit remain (see `TODO.md` top).
Anything below that says "origin/main is at f663ee0" or "cleanup is the only unpushed work" is stale.

## TL;DR

**Latest — `feat/presets-history` (built, NOT committed/merged):** Develop **Presets + edit-History +
Lightroom preset import**. New **`core-preset`** crate (pure CPU, **no wgpu**) holds a `serde_json::Value`-level
**sparse merge engine** (`apply_sparse` + amount-blend), a `ModuleScope` group→field map (Rust source of
truth, mirrored by `src/lib/presetScope.ts`, drift-guarded), and a format-agnostic import `Registry`
(`PresetImporter` trait) with **Lightroom `.xmp`** (roxmltree, `crs:` ns) + **`.lrtemplate`** (minimal Lua
parser) importers → a `PresetIr` → an honest `ImportReport{mapped,approximated,dropped}`. **Presets are
sparse per-field** (store only touched top-level `DevelopParams` fields → applying never resets
`toneAmount=100`/existing masks); typed round-trip happens in `src-tauri` so the parser never pulls in the
GPU stack. **LR fidelity = best-effort:** absolute WB Kelvin + color-grade/split-tone are **dropped**
(no anchor / incompatible `cb_rgb` gain-power channels), basic-tone sliders **approximated**, HSL 1:1,
tone-curve `/255`. **History = hybrid:** in-memory session undo/redo (⌘Z/⌘⇧Z, burst-coalesced) +
persistent named **snapshots** (DB). DB migrations **`015_presets`** + **`016_develop_snapshots`** (latest
schema = 16); 5 bundled built-ins seeded at setup. New left `DevelopSidePanel` (Presets | History tabs)

- create dialog (module checklist + masks caveat) + import-report modal + hover live-preview + copy/paste
  (⌘⇧C/V). **All headless gates green** (`cargo test --workspace`, `fmt --check`, `tsc`, `npm run build`;
  new code clippy-clean) + **Tier-1 mock UI QA passed** (panel renders, create/apply/undo, 0 console
  errors). **Hardening pass DONE (2026-06-26, headless-green — write-time validation, XMP scoping +
  element form + size cap, Lua depth-guard/long-brackets/schemaVersion gate, drag-clobber guard,
  built-in self-check + merge tests, PV-migration seam). Pending: in-app GPU/CR3 QA + commit.** Plan: `~/.claude/plans/act-as-senior-software-purrfect-glade.md`;
  deep notes: memory `darkroom-presets-history`; granular next (verify/harden/extend): `TODO.md` top section.

**Previously — `chore/cleanups-viewport-histogram` (MERGED `01a7b84`, not pushed):** a tech-debt pass. (1) Shared
`src/lib/useViewport.ts` hook (+ `src/lib/canvasPaint.ts`) extracts the ~200 LOC of canvas-viewport
logic duplicated between `Stage.tsx` + `Library/Loupe.tsx` (behavior-preserving). (2) **Whole-crop
histogram**: new `develop_histogram` IPC renders the full crop `{0,0,1,1}` at 384² (correct while
zoomed); `develop_render` no longer emits the viewport-biased one. (3) Doc reconciliation. `npx tsc`
clean; `cargo test`/`clippy`/`npm run build` green; in-app visual QA pending. Plan:
`~/.claude/plans/do-thorough-analysis-of-velvety-hollerith.md`.

**Recently MERGED to `main`** (NOTE: superseded — see "Repo state sync" at top; `main` is now `f7445df`
with Windows packaging / dedup redesign / logging / v0.1.1 also merged): the two separate
on-device AI passes — object detection (auto-after-import) + face recognition (manual "Find People") —
are now ONE manual scan (`feat/unified-ai-pipeline`, `f663ee0`: single shared decode, per-stage
dirty-DAG, deferred captions, data-safe face reconcile; **in-app GUI QA still pending**); plus
**capture-date ordering + keyset pagination + live-import dedup + live sidebar**
(`feat/import-ordering-keyset-paging`, `595685d`, migration `011`). Details: "Latest work — Unified AI
pipeline" below; designs in memory `darkroom-unified-ai-pipeline` + `darkroom-library-tree-staged-import`.

V1 is **functionally complete**, plus several post-V1 passes — most recently **develop fidelity: the
base tone curve fit to the real Adobe Camera Raw default + a Color-balance-RGB grading module**
(`feat/acr-curve-colorbalance`, merged `d3e1d3e`). Working space is linear wide-gamut **ProPhoto**;
develop has Kelvin WB (Planckian+Bradford CAT), exposure/contrast/highlights/shadows/blacks/whites,
tone curve, 8-band HSL, Detail (sharpen + luma/color NR), Lens vignette, **crop/straighten**, the
**scene-referred ACR-fit base tone operator** (mid-grey 0.18→0.388 ≈65% sRGB), **Color-balance-RGB**,
local masks (parametric/radial/brush/range), and **full-res viewport render** (canvas + view-rect).
`cargo test --workspace` + `clippy --workspace --examples` + `npm run build` all clean. **Caveat:** the
"240 CR3" validation is dev-machine-only — only 1 CR3 is committed; GPU/real-CR3 tests skip without the
fixture/Metal. **Biggest pending item: in-app visual QA** (`npm run tauri dev`) of the develop look on
varied real photos — the math is verified headless, but the ACR brightness / grading / crop _feel_ is
subjective (`BASELINE_GAIN` in `params.rs` is the one brightness knob).

## Latest pass — HDR: Canon PQ HEIF (.hif) + Merge-to-HDR (2026-07-18)

Branch `claude/hdr-heif-support-5r5ztx`. Plan: `~/.claude/plans/i-need-you-to-compiled-wirth.md`.

- **`.hif` decode (Feature A, full develop):** `core-raw/src/heif.rs` (ALL libheif calls isolated
  there, mirroring the rawler rule; `libheif-rs =2.7.0` `v1_17`, cfg'd out on Windows with a clean
  error stub). Chain: 10-bit HEVC 4:2:2 → libheif upsample+YCbCr→RGB (`HdrRgbLe`) → **PQ EOTF** via
  per-code LUT → linear BT.2020 scaled so **BT.2408 diffuse white (203 nits) = 1.0** (speculars ≈49×
  headroom; the single calibration knob `HDR_DIFFUSE_WHITE_NITS` in `core-raw/src/color.rs`) →
  Bradford D65→D50 + BT.2020→ProPhoto → `LinearImage`. Strict nclx gate (non-BT.2020-PQ → clean
  error). libheif applies container transforms → EXIF orientation deliberately NOT re-applied.
  Exif via the container's metadata block → kamadak `read_raw` → shared `meta_from_exif`.
  `ImageKind::{Heif,Hdr}` added; **`is_display` still means strictly JPEG/PNG** (HEIF/EXR are
  scene-referred, base tone operator active) — internal dispatch switched to `match classify`.
- **Merged-HDR storage:** `core-raw/src/hdr_file.rs` — fp16 RGB ZIP16 EXR **in the working format**
  (linear ProPhoto D50); reading back is a channel copy. Self-describing attrs: `chromaticities`
  (ProPhoto+D50), `darkroom:meta` (reference RawMeta JSON → `read_metadata`/`process_file` catalog a
  merged file with zero plumbing), `darkroom:sources` (ids/hashes/relative EVs + reference index).
  `.part`+rename durability. **No DB migration** (format TEXT takes `'heif'`/`'hdr'`).
- **Merge math (Feature B, tripod v1):** new leaf crate **`core-hdr`** — EV₁₀₀, `relative_scale`,
  median-EV reference, streaming `MergeAccumulator` (hat weights on each frame's own unscaled
  max-RGB, zero near clip, floored shadows; scaled-shortest-exposure fallback where all frames
  clip; ~7 f32/px any N). `core-raw` adds `read_exposure_numeric` (rawler rationals) +
  `develop_linear_wb` (reference-WB override so auto-WB brackets don't color-shift).
- **IPC:** `hdr_merge(image_ids) → ImageRow` (single-flight `AtomicBool`, ImportGuard around the
  write, validates 2–9 present RAW frames + varying exposure; dest `library_root/YYYY/YYYY-MM-DD/`
  else next-to-reference, `{ref_stem}_HDR.exr` via `unique_dest`). Events `hdr:progress
  {done,total,stage}` / `hdr:done {image}` / `library:changed`. FE: `useHdrMerge` hook, SelectionBar
  "Merge to HDR" + palette row, progress pill, HDR grid chip, format-aware TopBar badge, HEIF/HDR
  file-type filter chips. `SUPPORTED_EXT` += `hif`, `exr`.
- **Validated here (Linux):** synthetic 10-bit PQ HEIF fixtures (committed; 4:2:0 AND
  **4:2:2 — the exact Canon codec profile, retiring risk R1 at codec level**) decode byte-exact
  through `develop_linear` (diffuse white ≈1.0, max ≈49.26); a committed real iPhone gain-map HEIC
  is rejected with the clean profile error (negative test); 51-file Nokia conformance survey via
  the now fault-tolerant `heif_gate` (35 decode, 16 clean container errors, 0 crashes; all 8-bit);
  EXR round-trip; all merge math tests; full merge plumbing on a fabricated ±2 EV bracket of the
  committed R7 CR3 (exiftool-rewritten ExposureTime; 3×32.3 MP decode → 107 MB EXR → read-back);
  clippy/fmt/tsc green; `cargo check -p darkroom` green (GTK + pip-onnxruntime shims).
  **Real-file round (2026-07-19, user's actual R7 HDR shot):** S1 PASSED on the real .HIF
  (container/nclx/Exif/thumbnails all as assumed; no crash, 10-bit decode exact); S2 calibrated —
  anchor now **HDR_DIFFUSE_WHITE_NITS = 300** (+0.572 EV measured vs the metered CR3; record in
  color.rs); real ±3 EV bracket merged with no ghosting vs the camera's own HDR composite.
  Optimizations landed from the analysis: embedded 10-bit thumbnail fast path for HIF
  thumbs/previews (~10× faster) + rayon-parallel PQ conversion (core-raw now depends on rayon).
  **Pending on the dev Mac:** in-app QA, `.dmg` dylib bundling (`bundle.macOS.frameworks`,
  `libheif.1.dylib` + `libde265.0.dylib`); portrait-.HIF irot check + clipped-highlight bracket +
  plain RAW+HIF pair when such fixtures appear.

## Latest AI work — Unified AI pipeline (branch `feat/unified-ai-pipeline`, MERGED `f663ee0`)

Merges the two on-device AI passes into ONE manual scan for **10k–100k libraries**. Coordinator =
`src-tauri/src/analysis.rs::run_pass`, one job (single `analysis_running` guard + `analysis_cancel`):

- **Single shared decode** — `core_raw::preview_with_orientation` decodes the embedded JPEG ONCE →
  native ≤1024 (object detectors) + EXIF-oriented ≤1536 (faces); byte-equivalent to the old separate
  `preview_image`/`oriented_preview` (proven by `crates/core-raw/tests/decode_once.rs`) → no model
  re-validation.
- **Per-stage dirty-DAG** — `core_library::stale_targets`/`stale_count`/`present_targets_after`
  (keyset-paginated, `status='ok'` gate, never OFFSET; each image runs only its STALE stages). Bumping
  one stage no longer re-runs all (incl. MegaDetector@~0.95 s) across the library.
- **Phase A** detection + faces → **clustering** → **Phase B** captions (deferred; Florence built
  lazily via `build_captioner`, kept out of the Phase-A memory peak, dropped after).
- **Key CV decision (Codex-reviewed reframe):** NO upstream person-gate — SCRFD self-gates
  ArcFace/clustering (a body-detection gate would miss portraits/headshots and save little). Faces
  auto-participate when enabled (`face_stage_enabled`, default on) AND models present — never an
  implicit 190 MB download.
- **Face data-safety** — `core_library::reconcile_faces` (IoU-match) REPLACES the destructive
  `insert_faces`: a re-scan preserves stable face id + `person_id` + confirmed/rejected + cover, and a
  face clustering assigned to a person is NEVER dropped. Face inference errors no longer become a
  "0 faces" success (they retry). Clustering is EXACT pairwise (0.45 threshold) — ANN
  (instant-distance HNSW) is the documented lever for >~200 k faces.
- **Migrations** `012` (`images(status,id)` keyset index) + `013` (`json_extract` clears suspect legacy
  zero-face `face_detection` markers).
- **IPC:** auto-after-import trigger REMOVED (fully manual); `faces_run`/`faces_cancel`/`faces_status`
  are thin shims over the unified pass; new `face_stage_enabled` / `set_face_stage_enabled` (Settings
  "Detect people" toggle). Progress + completion ride ONE `analysis:*` stream (`useFaces` rewired off
  `faces:*`; `faces:models` kept for downloads).

Built in 6 phases (0–5) + a 3-aspect review (R1 correctness/data-safety, R2 perf/scale, R3 clean-code;
Codex was usage-limited so Claude review agents ran — **Codex cross-check still pending**). Regression
tests: `reconcile_keeps_person_assigned_unconfirmed_face`, `mismatched_dim_face_excluded`,
`json_extract_targets_only_zero_face_markers`, `decode_once`, `stale_targets_*`.

**Pending:** (1) **in-app GUI QA** (`npm run tauri dev`) — one scan does detection+faces+captions; one
progress bar; People populate before captions; a confirmed/assigned face survives a re-scan; cancel +
`faces_delete_all`-during-scan behave. (2) **Codex cross-check** — re-run the 3 Codex agents after the
OpenAI usage limit resets (~Jun 23 00:44); fold findings in. (3) **commit the branch.** (4) Deferred:
full Phase-A/B `run_pass` fn-split (cosmetic), ANN clustering for >200 k faces. NOTE: this work adds NO
GPU bindings — the develop pipeline's "next free = 15" is unchanged.

## Latest pass — ACR tone-curve fit + Color-balance-RGB (develop fidelity)

Branch `feat/acr-curve-colorbalance` (merged to main, `d3e1d3e`). Plan:
`~/.claude/plans/act-as-senior-software-moonlit-zephyr.md`. Deep notes: memory
`darkroom-acr-curve-colorbalance`. Began with a 9-agent audit that corrected the handoff docs (they
lagged by two merged branches: crop/tone-operator/import-lock/AI were already done).

**A. Base tone curve fit to the REAL Adobe default.**

- `crates/core-pipeline/src/base_curve_ref.rs` embeds Adobe's **universal default tone curve** (1025
  pts, from RawTherapee `adobe_camera_raw_default_curve` in `dcp.cc`). KEY FINDING (via `exiftool`):
  the on-disk `Canon EOS R7 Adobe Standard.dcp` has **no embedded ProfileToneCurve** → the R7 renders
  through exactly this universal curve. (DCPs live in `/Library/Application Support/Adobe/CameraRaw/
CameraProfiles/`.)
- User chose **match-ACR-brightness** → `params.rs::base_curve_value`/`acr_curve` map mid-grey
  **0.18 → 0.388 display-linear** (≈65% sRGB, L\*68.6), ~+1.3 EV brighter than the old 0.18→0.18.
  `tone_amount` (Base curve slider) blends a flat Reinhard (amount=0) → this ACR fit (amount=1, default).
- **Highlight shoulder (Codex/GPT-5.5 fix):** above x=0.875 the curve follows an asymptotic shoulder
  `1−(1−y0)/(1+a(x−x0))` (x0=0.875, y0=0.97702, a=10.468) — C¹ at the joint, asymptotes to 1.0, no hard
  clip corner (avoids highlight banding). The `1−k/(x+k)` form I first planned can't pass through (1,1).
- **`BASELINE_GAIN`** (`params.rs`, default **1.0**, rides the unused `ExtraUniform.texel.z`, applied
  scene-linear before grading+curve) is the single visual-QA brightness knob. `examples/measure_midgrey.rs`
  reports where mid-grey lands (fixture geomean ≈0.086, but that's the scene key — NOT a calibration
  target; the curve's 0.18→0.388 mapping carries the match since the buffer is white-normalized).
- Tests: `acr_fit_tests` (RMS L\* < 2.0 vs 16 ref points, via the real LUT-resample path) + updated
  golden `param_effects::base_curve_tone_response` (0.18→8-bit 167). `PROCESS_VERSION` 3→4.

**B. Color-balance-RGB (`@binding(14)` `CbRgbUniform`)** — faithful SUBSET of darktable `colorbalancergb`.

- Runs scene-linear, after develop+masks, **before** the base tone operator, in **Filmlight/Kirk grading
  RGB (D65)**. The ProPhoto⇄grading matrices are built in Rust (`params.rs::grading_matrices`, reusing
  `XYZ_TO_PROPHOTO_D50` + `mat3_inv/mul` + `bradford_cat`), **GPT-5.5-verified** (round-trip 7e-17, cond
  3.88), and shipped through the uniform (no magic WGSL constants). Grading RGB is NOT neutral-preserving.
- 4-way: global offset / shadows lift / highlights gain / midtones per-channel power (sign-aware,
  NaN-safe), each tonal-masked by darktable's exact `opacity_masks` (alpha/beta/gamma; weights 4/4/8,
  fulcrum 0.5). Plus scene-linear contrast + global chroma. **`CbRgb::is_identity()` → `params.z` active
  flag: at defaults the shader skips the whole grading round trip → byte-identical render** (goldens
  unaffected). `CbRgb` on `DevelopParams` (`#[serde(default)]`).
- Frontend: `ColorBalance.tsx` panel (4 zones × R/G/B + contrast/sat, −100..100 UI), wired through
  `useDevelop::onColorBalanceChange` → `InstrumentPanel` "Color balance" module.
- **Deferred tail:** JzAzBz/dtUCS perceptual saturation + brilliance (needs PQ EOTF), per-band sat/
  brilliance, hue-shift, vibrance, gamut LUT.

**Quick win:** eyedropper disarmed during crop mode (`MaskOverlay.tsx`). **New harnesses:**
`examples/{measure_midgrey,cb_demo}.rs`. **Codex review** was opted-in + run read-only from plan mode
(`workspace/logs/codex-curve-review.out`, gitignored; prose summary didn't flush but the numeric
matrices + tail computations stand and are folded in).

## Prior pass — viewport render (full-res zoom + near-instant edits + mask overlay)

Branch `feat/viewport-render` (merged to main). Render only the **visible viewport at display
resolution** (RapidRAW pattern): a `<canvas>` viewer + a server-side **view-rect** replace the old
`<img>` + CSS `transform: scale`, which on WKWebView rasterized at fit size and upscaled the bitmap
(blurry/glitchy zoom). **Mask-layer caching** skips the full-res mask pre-pass on pan/zoom/scalar
edits; **raw-RGBA** transport drops the 32 MP JPEG encode. **~260 ms → ~5 ms** per masked slider edit.

- **Backend (`core-pipeline`):** `ViewUniform` **`@binding(13)`** + `ViewParams`;
  `DevelopPipeline::render_view(ctx, prep, params, &ViewParams)` renders a crop-local viewport into a
  display-sized target. `render()` is a **byte-identical identity wrapper** (all 37 callers/goldens/
  export unaffected). Geometry split: `crop_to_source` in `develop.wgsl` (crop+zoom+straighten compose
  without double-fitting). **Mask cache** lives in `PreparedImage` (`mask_layer_hash: Mutex<Vec<…>>`),
  dirty key = `mask::mask_geometry_hash` (components/brush only, NOT scalars). Red overlay = one shader
  `mix` on the packed mask layer. New tests: `tests/viewport.rs` (5 Codex vectors) + a mask-cache
  correctness test; **all goldens byte-identical**; `bench_render` example (Codex-validated).
- **IPC (`src-tauri`):** `develop_render(image_id, params, view{ox,oy,sx,sy}, out_w, out_h,
overlay_mask_index, request_id) -> Response` returns raw bytes `[outW u32 LE][outH u32 LE][rgba]`
  (empty = superseded). `packed_overlay_layer` resolves the frontend mask index → packed enabled GPU
  layer. Output dims **clamped to 8192** (overflow guard). The preview-tier LRU (`DevelopLru`) was
  removed; develop_render always uses the **full-res** cached source for crisp zoom.
- **Frontend (`src/`):** `lib/viewport.ts` (view-rect math; `deriveViewRect` uses the per-axis-`min`
  model — correct for any container/image aspect). Canvas `Stage` + `Loupe` (no CSS scale); overlays
  map normalized↔px through the view rect; readout shows **true sensor dims** + %-of-1:1. Single-flight
  rAF render coalescing, double-buffered paint, crop-aspect-correct `natural`, and a `renderTick`
  (useDevelop → Stage) so **slider edits paint live** (not only on the next zoom).
- **Verified:** 41 core-pipeline tests green; goldens byte-identical; `clippy` clean; `npm run build`
  clean; Tier-1 mock visual QA (canvas renders, wheel-zoom 11%→88%, live exposure edit at +3 EV
  without zooming). Reviewed by 2 code-reviewer agents + 2 Codex passes; all Critical/High fixed.

### Gotchas / known limitations (read before extending)

- **Native GPU surface (zero-readback CAMetalLayer present) is NOT built** — deferred; needs the real
  app to validate macOS transparency/z-order/flicker. The canvas path already delivers full-res,
  glitch-free, near-instant edits, so it's a perf-polish. Full design: `~/.claude/plans/
snoopy-floating-island.md` (Workstream B; B0 is the go/no-go spike).
- **Shared viewport hook DONE** (`chore/cleanups-viewport-histogram`): `src/lib/useViewport.ts` +
  `src/lib/canvasPaint.ts` now own the canvas/view-rect/single-flight logic; `Stage.tsx` and
  `Library/Loupe.tsx` consume it (Stage injects crop fit-lock via `transformViewState`; Loupe keeps its
  tiered preview/decode render body). No more ~200 LOC dup.
- **Whole-crop histogram DONE** (same branch): `develop_histogram` IPC renders the full crop at 384²
  (correct while zoomed); `develop_render` no longer emits a viewport-biased histogram. Frontend
  triggers it on param/before-after change + first warm render (skip-if-cold avoids a duplicate decode).
- **First image open decodes full-res** (no preview tier in develop_render) — masked by the instant
  embedded preview. Tiered source (preview-res for fit, full-res on zoom) is a deferred optimization
  (Codex #3 — also the cheapest fix for fit-view minification aliasing).
- **Real-app visual QA still pending** — the Tier-1 mock is a synthetic gradient. Confirm full-res
  crispness, the red overlay COLOR over a real mask, and edit snappiness with `npm run tauri dev`.

## How to run / build / test

```bash
npm install
npm run tauri dev            # runs app; first launch auto-indexes library/2026/ into app data dir

cargo test --workspace       # 7 integration + unit tests (decode/index/pipeline/import/dedup)
cargo clippy --workspace     # clean
npm run build                # tsc + vite (frontend)

npm run tauri build -- --bundles dmg
# → target/release/bundle/dmg/Darkroom_0.1.0_aarch64.dmg  (ad-hoc signed, not notarized)
```

App data (catalog + thumbs) lives at `~/Library/Application Support/com.andrejvysny.darkroom/`
(`catalog.db` is WAL — rows are in `catalog.db-wal` until checkpoint).

Standalone validation harnesses (no GUI needed):

```bash
cargo run -p core-raw      --example decode_gate      # rawler decodes R7 CR3 (8/8)
cargo run -p core-library  --example scan_library      # index all 240, verify thumbs (~2s)
cargo run -p core-pipeline --example render_one        # decode → GPU develop → PNG (/tmp/darkroom-dev-*.png)
cargo run -p core-pipeline --example export_full       # full-res export → /tmp/darkroom-export.{png,jpg}
```

## Architecture

Cargo workspace (root `Cargo.toml`) — members: `src-tauri` + `crates/*`. Frontend at repo root `src/`
(deviates from spec's `/ui` intentionally, to reuse the scaffold).

| Crate           | Role                                                                                                        | Key files                                                                            |
| --------------- | ----------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ |
| `core-db`       | SQLite catalog: full DDL (STRICT), migrations, pragmas. Re-exports `rusqlite`.                              | `src/lib.rs`, `migrations/001_init.sql`                                              |
| `core-raw`      | rawler decode, embedded thumb/preview, EXIF meta, BLAKE3 hash, capture fingerprint, **linear develop**      | `src/{develop,meta,thumb,hash}.rs`                                                   |
| `core-library`  | indexing (rayon), thumb cache, queries, culling, edit persistence                                           | `src/{index,query,thumbs,cull,edits}.rs`                                             |
| `core-pipeline` | **wgpu/Metal develop pipeline** (WGSL, prepare/render), PNG/JPEG encode                                     | `src/{backend,params,encode}.rs`, `src/develop.wgsl`                                 |
| `core-import`   | copy/move/reference import, date routing, verify, Trash                                                     | `src/lib.rs`                                                                         |
| `core-dedup`    | byte + capture grouping, safe resolve→Trash                                                                 | `src/lib.rs`                                                                         |
| `core-preset`   | **(NEW)** sparse merge engine + format-agnostic preset import (LR `.xmp`/`.lrtemplate`). Pure CPU, no wgpu. | `src/{apply,scope,ir,map,registry,report}.rs`, `src/formats/{lr_xmp,lr_template}.rs` |
| `src-tauri`     | IPC commands, `thumb://` protocol, managed state                                                            | `src/{commands,protocol,state,lib}.rs`                                               |

**Frontend** (`src/`): `App.tsx` → `TopBar` + (`LibraryView` | `DevelopView`) + `CommandPalette` + `Toast`.
State in `store/app.ts` (zustand). IPC wrappers in `lib/ipc.ts`. Library data hook `lib/useLibrary.ts`;
develop hook `views/Develop/useDevelop.ts`; culling `hooks/useCulling.ts`; flows `lib/{export,importFlow}.ts`.
Views: `views/Library/{LeftNav,ThumbGrid,RightInfo,BottomBar,Loupe,DedupModal}.tsx`,
`views/Develop/{Stage,InstrumentPanel,Slider,Module,ToneCurve,ColorMixer,Histogram,Filmstrip}.tsx`.

### IPC command surface (the contract; all `invoke` snake_case)

- Library: `app_default_library`, `app_library_root`, `library_query`, `library_count`,
  `library_folders`, `image_meta`, `library_index_root`
- Develop: `develop_get_edit`, `develop_set_edit`, `develop_render` (viewport render → **raw RGBA**
  `[outW u32 LE][outH u32 LE][rgba]`, NOT JPEG), `develop_preview_jpeg` (instant first paint),
  `develop_get_histogram` (pull), `develop_histogram` (whole-crop pass → emits `develop:histogram`),
  `image_histogram` (Library panel)
- Presets (**NEW**, `feat/presets-history`): `presets_list`, `presets_get`, `presets_save`,
  `presets_update`, `presets_delete`, `presets_duplicate`, `presets_apply(image_id, preset_id, amount,
replace_all) → DevelopParams` (merged, NOT persisted — FE commits), `presets_export(id, dest)`,
  `presets_import_file(src) → {preset_id, report}`, `develop_apply_settings` (copy/paste, sparse-merge)
- History / snapshots (**NEW**): `snapshots_list`, `snapshot_create`, `snapshot_restore` (→ params,
  FE commits), `snapshot_rename`, `snapshot_delete`. Session undo/redo is **frontend-only** (no IPC).
- Export: `export_image`
- HDR: `hdr_merge(image_ids) → ImageRow` (Merge-to-HDR, tripod v1; events `hdr:progress
  {done,total,stage}` + `hdr:done {image}` + `library:changed`)
- Culling: `cull_set_rating`, `cull_set_flag`, `cull_set_label`,
  `cull_set_rating_many`, `cull_set_flag_many`, `cull_set_label_many` (batch)
- Keywords: `keywords_list`, `keywords_for_image`, `keyword_add_to_image`,
  `keyword_add_to_images` (batch), `keyword_remove_from_image`, `keyword_delete`
- Collections: `collections_list`, `collections_for_image`, `collection_create`,
  `collection_rename`, `collection_delete`, `collection_add_images`, `collection_remove_images`
- Dedup: `dedup_scan`, `dedup_resolve`
- Panorama merge: `panorama_preview`, `panorama_merge`, `panorama_cancel`, `panorama_status`,
  `panorama_preview_release` (drop the dialog's frame cache on close). Events:
  `panorama:{progress {phase}, done {imageId}, error}`
- Panorama detection (**NEW**, 2026-07-19): `pano_detect_run(force)`, `pano_detect_cancel`,
  `pano_detect_status` → `{running, suggested}`, `pano_detect_groups(include_dismissed)` →
  `PanoGroupRow[]`, `pano_detect_dismiss(group_id, dismissed)`,
  `pano_detect_mark_merged(group_id, merged_image_id)`. Events:
  `pano_detect:{progress {phase,done,total}, done {found}, error {message}}`
- Import: `import_start`; staged flow — `import_list` → `import_dedup` → `import_thumb` →
  `import_commit(source, mode, dest, selected, options, pairing)`
- RAW+JPEG pairing (**NEW**, schema 21): `import_commit`'s `pairing` ∈ {`"pair"`,`"standalone"`}
  (default standalone) links each camera companion (a `.jpg`/`.jpeg`/`.hif` sharing a RAW's folder +
  stem) to that RAW in `image_pairs`; `image_pair(image_id) → PairInfo|null`,
  `image_pair_unlink(secondary_id)` (emits `library:changed`). Linked companions are hidden from
  `library_query`/`library_count`/`list_folders`/`date_tree` unless `QueryParams.include_paired`, are
  excluded from every dedup detector, and `ImageRow` carries `paired_count` / `paired_to`.
- AI scan / People (**unified manual pass** — `feat/unified-ai-pipeline`): `analysis_status`,
  `analysis_run(force)`, `analysis_cancel`, `analysis_models_ensure`, `analysis_facets`,
  `analysis_detector_size`/`set_analysis_detector_size`, `face_stage_enabled`/`set_face_stage_enabled`,
  `image_detections`, `image_caption`; People — `faces_run`/`faces_cancel`/`faces_status`/
  `faces_models_ensure` (now **shims** over the unified pass), `people_list`, `person_faces`,
  `image_faces`, `person_set_name`/`person_set_hidden`/`person_set_cover`, `person_merge`,
  `face_confirm`/`face_reject`/`face_assign`, `faces_delete_all`. Events: `analysis:{models,progress,done}`
  (single stream — `{phase:"detect"|"caption",done,total}`) + `faces:models` (download only).
  `library_index_root`'s `analyze` flag is now a **no-op** (scan is fully manual).

`QueryParams` filter dimensions: `folder_id`, `min_stars`, `flag`, `color_label`
(`"__none__"` = unlabeled), `keyword_id`, `collection_id`, `import_session_id`, `include_paired`
(view option, not cleared by "All photos"), `search`
(filename/camera/lens/keyword), `sort` ∈ {capture_desc|asc, filename|\_desc, rating_desc|asc,
imported_desc|asc}.

- Protocol: `thumb://localhost/<content_hash_hex>?size=N`
- Events: `import:progress {done,total}`, `import:done {ImportStats}`

### Data flow

- **Thumbnails:** `core-raw` extracts embedded preview JPEG → downscale 512px → disk cache keyed by hash → `thumb://` protocol → `<img>`.
- **Develop:** `core-raw::develop_linear` (rawler demosaic + our own camera→**linear wide-gamut ProPhoto** map via `clip_negative`, keeping >1.0 highlight headroom) cached once per image at FULL res (`prepare()` uploads to an `Rgba32Float` texture); slider/zoom/pan → `render_view()` renders only the visible crop-local viewport at display res (uniform rewrites + draw + small readback) → **raw RGBA bytes** → `ipc::Response` → JS paints a `<canvas>` (see "Prior pass — viewport render"). The shader does scene-linear edits → Color-balance-RGB → base tone operator → ProPhoto→sRGB at the display transition. Export re-decodes full-res → full-res render → PNG/JPEG.
- **Export:** re-decode full-res → `render_once` (full-res GPU) → PNG/JPEG → save dialog dest.

## Critical technical facts / gotchas (verified against installed crate sources)

- **rawler `=0.7.2`** (pinned, non-SemVer; ALL rawler calls isolated in `core-raw`).
  - `rawler::decode_file(path) -> RawImage`; `rawler::decode(&RawSource, &RawDecodeParams)`.
  - `rawler::analyze::extract_{thumbnail,preview}_pixels(path, &params) -> DynamicImage`.
  - Metadata WITHOUT pixel decode: `get_decoder(&src)?.raw_metadata(&src, &params)? -> RawMetadata{exif, lens, …}`.
  - **Linear develop:** `rawler::imgop::develop::RawDevelop { steps: [Rescale, Demosaic, CropActiveArea, WhiteBalance, Calibrate, CropDefault] }` (omit `SRgb`) → `develop_intermediate(&RawImage) -> Intermediate::ThreeColor(Color2D<f32,3>)`. This does demosaic + color matrix for us — no hand-rolled color code.
- **wgpu 29** API (differs a lot from older versions):
  - `Instance::new(InstanceDescriptor::new_without_display_handle_from_env())` — by value.
  - `request_adapter`/`request_device` return `Future<Output=Result<…>>` → `pollster::block_on`; `request_device` yields `(Device, Queue)`.
  - `PipelineLayoutDescriptor.bind_group_layouts: &[Option<&_>]`; field `immediate_size` (no `push_constant_ranges`).
  - `RenderPipelineDescriptor`/`RenderPassDescriptor`: `multiview_mask: Option<NonZeroU32>` (not `multiview`).
  - `SamplerDescriptor.mipmap_filter: MipmapFilterMode`.
  - Copy types `TexelCopy{Texture,Buffer}Info` + `TexelCopyBufferLayout`.
  - OOM handling: `let g = device.push_error_scope(ErrorFilter::OutOfMemory); … ; pollster::block_on(g.pop())`.
  - `device.poll(wgpu::PollType::wait_indefinitely())`; buffer map via `buffer.slice(..).map_async(MapMode::Read, cb)`.
- **GPU uniform layout** (`ParamsUniform` in `params.rs` ↔ `Params` in `develop.wgsl`): `vec3 wb_gain` + `f32 exposure` packs correctly (exposure at byte offset 12; std140/WGSL places a scalar in the vec3's tail). A code review FALSE-flagged this as misaligned — **do NOT add padding** (it would break it). Guarded by golden test `crates/core-pipeline/tests/param_effects.rs`.
- **SQLite versions:** `rusqlite 0.39` + `rusqlite_migration =2.5.0` pinned — newer needs rustc ≥1.95 (we have 1.91), and 0.39/2.5 share `libsqlite3-sys 0.37`. `core-db` re-exports `rusqlite` so every crate links the same one (avoids `links=sqlite3` conflicts).
- **Develop preview delivery:** command returns `tauri::ipc::Response::new(jpeg_bytes)` → JS `invoke<ArrayBuffer>` → `URL.createObjectURL(new Blob([buf],{type:'image/jpeg'}))` (revoke old URL). Never base64-over-IPC.
- **CSP is `null`** in `tauri.conf.json` (permissive) — `thumb://` + inline styles work; harden before public distribution.
- **`app_default_library()`** uses `env!("CARGO_MANIFEST_DIR")` → only resolves on the build machine (auto-bootstraps `library/2026` in dev); returns `None` elsewhere (user adds folders via Import).

## Done / Partial / Not done

**Presets + Edit-History + LR import (NEW — branch `feat/presets-history`, built + headless-verified +
Tier-1 mock UI QA, NOT committed):** sparse-per-field presets (DB `presets`, 5 built-ins) with
create/apply/amount/duplicate/export/import + copy-paste settings; hybrid history (session undo/redo +
DB snapshots); Lightroom `.xmp` + `.lrtemplate` import with an honest `ImportReport`. New `core-preset`
crate (no wgpu), migrations 015/016, left `DevelopSidePanel`. Adds **no GPU bindings** (the develop
pipeline's "next free = 15" is unchanged). **Pending: in-app GPU/CR3 QA + commit** — see `TODO.md` top
("verify / harden / extend") and memory `darkroom-presets-history`.

**Done & validated:** catalog + indexing + thumbnails; Library grid/nav/metadata; GPU develop (WB,
exposure, contrast, highlights, shadows, saturation, blacks, whites) + edit persistence; culling
(rating/flag/label + keyboard loop); ⌘K palette + shortcuts; loupe zoom/pan; export PNG/JPEG; import
(copy/move/reference); dedup (byte+capture) + resolve; `.dmg`.

**Phase 1 wired (NEW, validated on real CR3):** Tone curve (LUT `@binding(3)`), HSL color mixer
(`FxUniform @binding(4)`), real before/after (`\`), real histogram (`develop:histogram` event),
per-module reset, library search bar. All new GPU data uses NEW bindings — `ParamsUniform`/`wb_gain`
alignment is untouched (`param_effects` still green). New golden tests: `tone_curve.rs`, `hsl.rs`,
`curve`/`histogram` unit tests. New files: `core-pipeline/src/{curve,histogram}.rs`.

**Library organization — DONE & validated (catalog-logic tested; UI builds clean):**
Filtering & sorting across stars/flags/color-labels (+ unlabeled), 8 sort orders, folder nav;
keywords/tags (full CRUD, per-image editor + autocomplete, batch tag, nav filter, keyword search);
static + smart collections (membership + saved-predicate, nav create/filter/delete, "save filters
as smart"); multi-select (cmd/shift) with a batch toolbar (rating/flag/label/keyword/collection/
export) + batch keyboard culling; import-mode picker (copy/move/reference); single + batch export.
Backed by `core-library/{query,keywords,collections,cull}.rs` (30 backend tests) and thin Tauri
commands; all SQL filters are bound named params (injection-safe).

**Develop fidelity (post-V1, wired + validated):** working space is now **linear wide-gamut
ProPhoto** ("Melissa RGB") — `core-raw::map_3ch_to_rgb` targets ProPhoto, `develop.wgsl` converts
ProPhoto→sRGB at the display transition. Scene highlight headroom preserved (`clip_negative`).
**Kelvin white balance** via Planckian locus (Kim 2002) + Bradford CAT on `@binding(8)` (GPT-5.5-
reviewed; `wb_matrix(0,0)` is exact identity). Independent endpoint blacks/whites. **Detail** (3×3
unsharp sharpen + luma/color NR) + **Lens vignette** on `@binding(9)`. **Scene-referred base tone
operator fit to the real ACR default** (`@binding(10/11)`, `base_curve_ref.rs`; mid-grey 0.18→0.388).
**Color-balance-RGB** 4-way grading (`@binding(14)`, Filmlight grading RGB). **Crop/straighten**
(`@binding(12)`). **Viewport render** (`@binding(13)`).

**Crop/straighten — DONE (visual-QA pending), as of `feat/tone-operator-crop`:** GeomUniform
`@binding(12)` + `crop_to_source`/`sample_bilinear` (the bilinear-remap "helper" the old note asked
for already exists, `develop.wgsl`), interactive `CropOverlay.tsx`, aspect presets + straighten slider,
export at true dims via `Crop::export_rect`. **Bindings 0–15 all used (15 = `ChanMix`
channel mixer); next free = 16.**

**Windows packaging — WIRED (branch `feat/windows-packaging`):** NSIS per-user `.exe` target
(`tauri.conf.json` `bundle.windows`), DirectML EP for AI (`core-analyze` per-target `ort` features +
cfg-gated `models.rs`; CoreML on macOS, DirectML on Windows, CPU fallback), `release.yml`
(tag-triggered macOS+Windows artifacts via `tauri-action`) + a `windows-build` compile gate in
`ci.yml` + `rust-toolchain.toml` (1.91.0). onnxruntime stays statically linked on Windows (no DLL to
ship); `DirectML.dll` is a Win10 1903+ system component. **Pending:** first green `windows-build` run +
manual end-to-end QA on a Windows box. Unsigned (SmartScreen warning); custom title bar still
macOS-only (Windows uses native decorations).

**Not done (deferred from spec):** keyword hierarchy UI, "recent import" as a true session filter,
per-display ICC, RCD/AMaZE demosaic, Linux, macOS notarization, Windows code-signing, CSP hardening.
(Thumbnail LRU eviction and FS-watcher reconciliation are DONE — see `thumbs.rs::evict_to` and
`src-tauri/watch.rs`.)

## Known issues / caveats

- `import_start` lock freeze is **RESOLVED** (ea0d66a): `core_import::import` takes `&Mutex<Db>` and
  brief-locks only the initial snapshot, per-file relink/insert, and session finish — copy/hash/
  thumbnail run unlocked between locks, so IPC stays responsive; the FS watcher is gated via an
  `ImportGuard` RAII (`src-tauri/src/watch.rs`). (`develop_render` likewise decodes + GPU-prepares
  unlocked, locking only the brief render+readback.)
- Loupe uses the 512px cached thumb upscaled (no dedicated larger preview yet).
- Export re-decodes full-res (≈1.6s) each time; not cached.
- Unsigned dmg blocked by Gatekeeper on other Macs (`xattr -dr com.apple.quarantine`).

## Suggested next steps (priority order)

See **TODO.md → top "DONE/NEXT" section** for the authoritative list. In short:

1. **In-app visual QA** (`npm run tauri dev`) — the #1 pending item. Confirm the brighter ACR default
   - Color balance panel + crop/straighten + Temp/Tint/Sharpen/Vignette on varied real CR3. Tune
     `BASELINE_GAIN` (`params.rs`, default 1.0) if the default look is too bright/dark. The math is
     verified headless; the look is subjective.
2. **Develop fidelity continuation** (now unblocked by the curve fit): Lightroom `.xmp` preset import
   (new `core-preset` crate); clarity/texture/dehaze (needs a multi-scale blur beyond the 3×3);
   color-balance perceptual tail (JzAzBz sat/brilliance, per-band, hue-shift, vibrance); grain /
   channel-mixer / HaldCLUT.
3. **Lens distortion / chromatic-aberration** (the only UI-absent geometric module; greenfield —
   reuse `sample_bilinear` for a radial UV / per-channel scale on a fresh binding).
4. **Viewport leftovers:** whole-crop histogram pass (`commands.rs` TODO); extract the shared
   Stage/Loupe `useViewport` hook (~200 LOC dup); tiered preview source; B0 native-GPU-surface spike.
5. Higher-leverage review items: dedup orientation-normalize before dHash; per-mask WB-as-CAT;
   bilateral (not box) NR; loupe ≥1536px preview; export full-res cache.
6. AI tail: ort dylib bundling (HIGH iff distributing a built `.app`), Florence-2 KV-cache,
   PresenceProbe calibration. Tests: `src-tauri`/`core-db`/`core-analyze` have 0 integration tests.
7. Pre-distribution only (de-scoped while personal): CSP hardening, command path-scoping, codesign +
   notarize.
