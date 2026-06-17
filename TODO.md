# Darkroom — TODO

> Continuation tracker. Full status + architecture + gotchas in `CURRENT_STATE.md`. Spec: `SPEC_V1.md`.

## DONE: Behavioral-signal capture (Phase 0 — labeled data for future AI)

> Plan: `~/.claude/plans/act-as-senior-ai-linked-peacock.md`. Captures decision/label signals so the
> four future models (dedup · best-shot · lighting · auto-edit) can train on real usage. The app
> previously kept only final state + discarded decision context. Compiles, clippy-clean, tests pass,
> real-data compute verified.

- [x] Migration `007_user_events.sql`: append-only `user_events` log + per-image `image_features`.
- [x] `core-library/events.rs` (`append_event`/`Event`/`ids_json`) + `features.rs`
      (`compute_features`: luma+log-chroma histograms, sharpness, clip/DR; `set_image_features`,
      `images_missing_features`). `core-raw::as_shot_wb` (as-shot WB coeffs).
- [x] `src-tauri/events.rs` (`stamp`/`log_event`) + `session_id`/`app_version` in `AppState` +
      `session.start` at setup.
- [x] Wired events into `cull_set_*` (+`_many`, latency/group/candidates), `develop_set_edit`
      (params before/after + touch_count), `export_image` (endorsement), and **`dedup_resolve`
      extended** to log candidate set + auto-keeper + override.
- [x] `features_backfill` pass + IPC + Settings "Compute features" button; `image_features` overwrite.
- [x] Frontend: ipc wrappers (optional ctx), `useDevelop` touch_count, `useCulling` latency.
- [x] `examples/export_training_data.rs` (per-feature JSONL), `features_one.rs` (real-data check),
      `tests/events_features.rs` round-trip.
- [ ] In-app smoke (`npm run tauri dev`): cull/edit/export/dedup → inspect `user_events`; run
      "Compute features" → inspect `image_features`. (Deferred — needs GUI.)
- [ ] FOLLOW-UP MODELS (deferred, consume the log): dedup keeper-ranking → best-shot → lighting
      normalization → auto-edit style. Training-time grouping for best-shot via `capture_fingerprint`.

## ACTIVE: AI People/Animal detection accuracy overhaul

> Plan: `~/.claude/plans/act-as-senior-ai-linked-peacock.md`. Root cause: D-FINE no-background sigmoid
> heads + 0.45 gate + no precision filters → false positives on empty frames. One integrated release.
> Architecture: D-FINE-M → People+Vehicles · MegaDetector-v5a → Animals · MobileCLIP-S1 → verify gate.

### WS1 — D-FINE precision fixes (no new models)

- [x] `coco.rs`: per-category `threshold()` (person 0.55, vehicles 0.50); `category()` → People/Vehicles
      only (drop Animals + `teddy bear`).
- [x] `detector.rs`: confidence floor (0.50) + margin gate (best < 1.5×second → reject); box-sanity
      (area 0.003–0.85; person aspect w/h ≤1.5; drop tiny edge-touching).
- [x] `models.rs`: detector `ModelFormat::MLProgram` + `static_input_shapes` (dynamic-dim model).
- [x] bump `DETECTOR_VERSION` → `dfine-m-coco-v2`.
- [~] EXIF orientation in `decode_srgb` — DEFERRED (preview may be pre-oriented; regression risk; nit).
- VALIDATED: 3/4 FP frames clean. `_55A4063` (poppy) still person@0.825 — WS3 verifier's job.

### WS5 — manual ground-truth labeling (feature + eval source) ✅ (compiles + tsc clean)

- [x] migration `006_labels.sql`: `image_user_labels(image_id PK, contains_person, contains_animal, updated_at)`.
- [x] core-library getter/setter (whitelisted col, bound params); IPC + `lib/ipc.ts`; checkboxes in `RightInfo.tsx`.
- [x] `examples/detect_eval.rs`: FP-regression mode via real ObjectDetector + prod decode path.
- [ ] extend `detect_eval.rs`: read labels from catalog.db → precision/recall (once positives labeled).

### WS2 — MegaDetector-v5a → Animals ✅ DONE (validated: dog@0.931, FP frames→0)

- [x] MDv5a ONNX I/O confirmed via `onnx_io`: `images[1,3,−1,−1]` (dynamic) → `output[1,N,8]`.
- [x] `megadetector.rs`: YOLOv5x6 letterbox(stride-square) + obj×cls decode + NMS; class 0=animal →
      Animals ("animal"); runs CPU (dynamic dims unsupported by CoreML EP); verifier-gated.
- [x] single **dynamic** ONNX (`md_v5a_dynamic.onnx`, MIT) serves both 1280²/640² — no dual download.
- [x] resolution setting via `app_meta` (`animal_detector_size`); IPC get/set; set invalidates registry
      cache; `ANIMAL_DETECTOR_VERSION_{1280,640}` encodes size.
- [x] registered in `registry()`; scoped projection (`project_detections` owns categories) so D-FINE
      (People/Vehicles) + MegaDetector (Animals) don't clobber each other.

### WS3 — MobileCLIP-S1 verifier ✅ DONE (validated: poppy rejected, people/dog kept)

- [x] MobileCLIP-S1 ONNX (`Xenova/mobileclip_s1`, MIT): vision (CoreML) + text (CPU, fixed 77-token).
- [x] `verify.rs`: precompute prompt embeds; crop(+20% pad)+cosine softmax gate (`VERIFY_ACCEPT=0.40`);
      gates People + Animals.
- [x] wired shared `Verifier` into ObjectDetector + MegaDetector.

### WS4 — query floor + UI ✅ DONE

- [x] confidence floor `>= 0.5` in `analysis_facets` + `detectedCategory` filter.
- [x] Settings: MD-resolution selector (1280/640) in `SettingsModal.tsx`.

### Verify ✅ (all green)

- [x] `detect_eval` (D-FINE+verifier): 0 People/Animals on the 4 FP frames; recall kept on people imgs.
- [x] `animal_eval` (MegaDetector+verifier): dog@0.931 (1280 & 640), FP frames → 0 animals.
- [x] `cargo test --workspace` (incl. updated analysis.rs fixture), `cargo clippy --workspace`, `tsc` — clean.
- [ ] CoreML CPU-vs-CoreML parity diff (deferred — thresholds now far from the FP16 boundary).
- [ ] e2e in-app `npm run tauri dev` ↺ re-analyze (needs ~900MB model download on first run).
- [ ] tune `VERIFY_ACCEPT`/prompts + MD threshold once user labels positive CR3s (WS5 eval harness).

## Leftovers / next (after the post-V1 develop-fidelity + review session)

> Develop fidelity (ProPhoto working space, scene-referred highlights, Kelvin WB CAT, endpoint
> blacks/whites, Detail sharpen/NR, Lens vignette) + data-safety fixes are DONE & on `main`
> (commits `442f547`→`b5b3eda`). What's left, prioritized:

- [ ] **Visual QA the develop pass in-app** (`npm run tauri dev`) — Temp/Tint, Highlights, Sharpen,
      NR, Vignette on real CR3. Math verified headless; _feel_ is subjective. Single-constant tunables:
      mired span `params.rs::white_xy` (±range), rolloff shoulder `develop.wgsl::highlight_rolloff`
      (`a=0.75`), highlight-mask threshold (`0.25`), NR/sharpen response, vignette `0.6` gain.
- [ ] **Geometric develop modules** (still UI-only) — Crop (aspect + straighten angle) + Lens
      manual distortion / chromatic-aberration. Need: a `sample_bilinear` 4-tap helper (input is
      non-filterable `Rgba32Float`), straighten = rotate+autozoom UV in-shader, crop = overlay rect
      in `Stage.tsx` + apply at export (avoid preview output-dims churn), distortion = radial UV /
      per-channel scale. All need **visual QA**. Crop UI = aspect buttons + Angle slider (unwired).
- [ ] **`import_start` lock refactor** — restructure `core_import::import` to brief-lock-snapshot →
      unlocked parallel copy/move/hash/thumbnail → brief-lock single-tx insert (mirror
      `library_index_root`/`run_pass`); + startup sweep for dangling sessions; + dedup seen-set
      `status='present'` filter & relink path. Ends the whole-import IPC freeze.
- [ ] **Higher-leverage review items:** dedup `dhash_from_jpeg` — normalize orientation before
      hashing (rotation-sensitive); per-mask WB as a CAT (currently per-channel gain delta);
      bilateral/edge-aware NR (currently a plain 3×3 box → softens edges); dedicated loupe preview
      (≥1536px, not upscaled 512 thumb); cache full-res developed buffer for repeat export.
- [ ] **Minor / honesty:** aspect-correct the linear gradient mask (`mask_prepass.wgsl::linear_cov`,
      needs FE+BE coord consistency); decide brush `flow` (wire buildup off MAX-blend, or remove from
      schema+UI); frontend nits (init `selectedId` to null not `6`; re-key `Stage` zoom/pan on
      `selectedId` not `imageUrl`; remove the fake `RightInfo` histogram + dead Filmstrip zoom/1:1).
- [ ] **Pre-distribution only** (de-scoped while personal/single-user): CSP hardening; canonicalize
      `export_image`/`import_start`/`library_index_root` dest/source/path against allowed roots in the
      Rust command layer; ort dylib bundling (`externalBin`/frameworks) so the AI feature loads in a
      built `.app`; Developer-ID codesign + notarize; tests for `core-analyze` + `src-tauri` (both
      currently 0 tests) + the highest-risk import/dedup branches (Move source-delete, copy
      hash-mismatch, stale-keeper resolve).

## V1 — DONE ✅ (all 5 acceptance criteria met + validated on real R7 CR3)

- [x] **Phase 0** Workspace, decode gate (8/8), `core-db` (DDL+migrations), app shell, Tauri wiring, dmg config
- [x] **Phase 1** `core-raw` + `core-library` indexing/thumbnails/queries, `thumb://`, Library UI (240/240 in ~2s; live render ✓)
- [x] **Phase 2** wgpu/Metal develop pipeline (WB/exposure/contrast/highlights/shadows/saturation/blacks/whites), ~2 ms/slider, edits persisted, Develop UI
- [x] **Phase 3** culling (rating/flag/label + keyboard loop), ⌘K palette + shortcuts, loupe zoom/pan
- [x] **Phase 4** `core-import` (copy/move/reference, date-routed, hash-verified) + `core-dedup` (byte+capture, Trash resolve) + UI
- [x] **Phase 5** export PNG/JPEG (full-res GPU) + dialog + ⌘E
- [x] **Phase 6** release `.dmg` (ad-hoc signed) — `Darkroom_0.1.0_aarch64.dmg` (checksum VALID)

Quality: `cargo test --workspace` (31 suites, all green) · `cargo clippy --workspace --examples` clean · `npm run build` clean.

## Local Adjustment Masks (in progress) — plan: `~/.claude/plans/act-as-expert-on-lucky-journal.md`

> LR component model · masks reuse global scalars as deltas · Range + guided-filter included · AI schema-only.
> Guard intact: `ParamsUniform`/`wb_gain` untouched — all mask data via NEW bindings 5–7 + storage buffer.

### Phase 1 — Backend refactor + schema (no behavior change) — DONE ✅

- [x] `params.rs`: mask schema (Mask/MaskComponent/ComponentKind/MaskOp/LocalAdjust/BrushStroke) + `masks: Vec<Mask>` on DevelopParams (`#[serde(default)]`)
- [x] `params.rs`: MASK_CAP=16, MaskParamsUniform, MaskBufferUniform, `to_mask_buffer()`
- [x] `develop.wgsl`: split `fs()` → `apply_local_linear`/`apply_local_display` (lossless); bindings 5–7 + count==0 guard
- [x] `backend.rs`: PreparedImage gains mask-alpha D2Array (R16Float RENDER_ATTACHMENT — not storage-bindable on Metal) + filtering sampler + MaskBuffer storage
- [x] `commands.rs`: PROCESS_VERSION 1→2 · TS: mirror `masks` in `ipc.ts` + store `freshDefaults`
- [x] Test: `tests/masks.rs` (packing + Phase-1 inertness); golden tests green = lossless refactor. clippy + tsc clean.

### Phase 2 — Parametric (linear+radial)

- [x] **Backend DONE ✅**: `mask.rs` (PrepassUniform/PrepassComponent/MaskPrepass) + `mask_prepass.wgsl` (linear/radial coverage + Add/Sub/Intersect composite). `backend.rs` runs pre-pass per enabled mask → alpha layer; develop loops composite. Test `full_coverage_mask_matches_global` proves end-to-end compositing == global. clippy/tests green.
- [x] **Frontend DONE ✅**: `lib/maskGeom.ts` (coord util + factories); store (`selectedMaskIndex`, `maskOverlayVisible`); `useDevelop` mask CRUD (add/update/delete/adjust/component-kind → commit); `MaskOverlay.tsx` SVG drag handles (linear endpoints, radial center+resize); `Stage.tsx` deterministic fit + wheel-zoom + drag-pan + overlay; `MaskPanel.tsx` (add/select/enable/invert/opacity + 9 adjustment sliders) in InstrumentPanel. tsc + `npm run build` clean.
- [ ] Note: pre-pass recomputes every render (cheap for parametric); brush dirty-cache deferred to Phase 3. Visual QA in Tauri app pending (user to run).
- [ ] Polish later: cursor-anchored zoom (currently center-origin), radial rotation handle, click-empty-to-deselect.

### Phase 3 — Brush — DONE ✅

- [x] Backend: `brush_bake.wgsl` (instanced dabs; paint=MAX blend, erase=multiply) + `BrushBake`/`flatten_strokes` in `mask.rs`; `bake_brush()` in backend.rs bakes per brush mask before its pre-pass; prepass samples brush coverage (binding 1). Test `brush_stroke_brightens_locally` green.
- [x] Frontend: brush settings in store; `newBrushMask`; `appendStroke`; `BrushLayer` in `MaskOverlay` (capture + live preview + committed-stroke preview); `+ Brush` + size/hardness/strength/erase sliders in MaskPanel. Strokes commit on pointer-up (coalesced).

### Phase 4 — Range + edge-aware refine — DONE ✅

- [x] Backend: `mask_prepass.wgsl` luma/color range coverage (samples input image, binding 3). `mask_refine.wgsl` separable cross-bilateral (luma-guided) — `MaskRefine` + `refine_pass()`; pre-pass→scratch_a, refine (feathered: H/V) or passthrough → alpha layer. Tests `luminance_range_selects_brights_only` green. (Bilateral form of edge-aware feather; full guided-filter He 5-step is a future swap-in.)
- [x] Frontend: `newLuminanceMask`/`newColorMask`; range sliders + eyedropper (`samplePixelHsv`, store `pickingColor`) + "Refine edges" toggle.

### Phase 5 — Combine + multi-mask polish — DONE ✅

- [x] Component combine (Add/Subtract/Intersect + invert) — math already in prepass; UI: per-mask Components list with op selector, active-component select, add/remove component buttons, `selectedComponentIndex` store, overlay + param sliders target active component. Test `component_intersect_narrows_coverage` green.

> All phases: `cargo test -p core-pipeline` (16 tests) + clippy clean · `tsc` + `npm run build` clean. Visual QA in Tauri app still pending (user to run).
> Deferred polish: brush dirty-cache (re-bakes every render), cursor-anchored zoom, radial rotation handle, full guided-filter, AI component impl.

## AI scan analysis (object detection + captioning) — in progress

> Plan: `~/.claude/plans/act-as-expert-on-tidy-wave.md`. Spike: `crates/core-analyze/SPIKE.md`.
> Modular `Analyzer` pipeline; background pass after scan; results in side-tables + separate "Detected/AI"
> panel (keywords untouched). License-clean: D-FINE (Apache) + Florence-2 (MIT) via `ort` + CoreML.

### Phase 0 — runtime + model spike — DONE ✅ (validated on real CR3 + COCO images)

- [x] `core-analyze` crate; `ort =2.0.0-rc.12` (coreml) builds on rustc 1.91; CoreML EP registers/runs.
- [x] D-FINE-S detector end-to-end: correct decode, ~108ms/img CoreML; validated (cats→2 cats, street→14 people).
- [x] Florence-2 captioner viable: non-merged decoder pair + `GraphOptimizationLevel::Level1` + f32 I/O (no half). Two ORT gotchas documented in SPIKE.md. usls rejected (alpha, ort pin mismatch).
- [x] Harnesses: `examples/detect_one.rs`, `examples/onnx_io.rs`.

### Phase 1 — analyzer engine (`core-analyze`) — DONE ✅ (validated end-to-end)

- [x] `Analyzer` trait + `AnalysisCtx`/`AnalysisRecord` + payloads + `AnalyzerRegistry` (`lib.rs`).
- [x] `ObjectDetector` (D-FINE, `detector.rs`): preprocess + sigmoid/argmax decode + IoU dedup + COCO→bucket. Validated (cats→2 cats, street→14 people).
- [x] `Captioner` (Florence-2, `caption.rs`): 4-session seq2seq, full-recompute greedy decode (with-past export fixes seq=16 → unusable), keywords = caption nouns ∪ prior detection labels. Validated: cats→"Two cats laying on a pink blanket with remotes."
- [x] `models.rs` first-run download/verify (min-size guard) + `build_session` (CoreML + per-model opt level).
- [x] Harnesses: `examples/{caption_one,analyze_one}.rs`. clippy clean.
- [ ] Note: download path (`ModelStore::ensure`) structured but runtime-tested in Phase 3 integration.

### Phase 2 — persistence — DONE ✅

- [x] Migration `005_analysis.sql` (003/004 already used by scale/phash): `analysis_results` (PK image×analyzer×version), `image_detections` (denorm + indexes), `image_captions`. Registered in `core-db/src/lib.rs`.
- [x] `core-library/src/analysis.rs`: `existing_analysis` (skip-set), `insert_analysis` (idempotent JSON→projections), `present_images`, read rows + `analysis_facets`. No ML/ort dep.
- [x] `query.rs`: `QueryParams.detected_category` + EXISTS subquery. Tests `tests/analysis.rs` green; clippy clean.

### Phase 3 — background analysis pass + IPC — DONE ✅

- [x] `src-tauri/src/analysis.rs` (orchestration lives in app layer, keeping `core-library` ML-free): `run_pass` (rayon decode→analyze→1-tx insert, version-gated skip, RAII running-guard, failure isolation), `ensure_models`, lazy `registry`, `status`, `decode_srgb` (preview→1024px). Detector bbox now normalized [0,1].
- [x] `state.rs`: `models_dir` + lazy `analyzers` + `analysis_running`. `commands.rs`: 6 commands (status/models_ensure/run/facets/image_detections/image_caption) + auto-trigger after index when models ready. Registered in `lib.rs`. App builds + clippy clean.
- [x] Validated: `ModelStore::ensure` downloads `dfine_m` (ureq) + D-FINE-M detects (cats→2 cats, normalized bbox). `examples/models_smoke.rs`.
- [ ] Full app-run (download Florence + analyze library in-app) deferred to verification.

### Phase 4 — frontend "Detected/AI" panel + LeftNav facet — DONE ✅

- [x] `src/lib/ipc.ts`: analysis types + 6 wrappers + `QueryParams.detectedCategory` (in FILTER_DIMENSIONS + clearedFilters).
- [x] `src/lib/useAnalysis.ts` (new hook): status/facets/progress/doneVersion + `analysis:models|progress|done` listeners + `triggerAnalysis`/`reloadFacets`.
- [x] `LeftNav.tsx`: "Detected" facet (People/Animals/Vehicles → `detectedCategory`) + Analyze/Re-analyze buttons. `RightInfo.tsx`: read-only Detected/AI panel (caption + keywords + per-category chips, race-safe). `LibraryView.tsx` wiring + progress overlay. `npm run build` clean.

### Phase 5 — gates — DONE ✅ (one manual step remaining)

- [x] `cargo test --workspace` green (incl. new analysis tests, no regressions); feature code clippy-clean; `npm run build` clean.
- [x] Release links `ort` under `panic=abort`+`lto`; **onnxruntime STATICALLY linked** (no dylib to bundle) + system CoreML.framework — big packaging win.
- [ ] MANUAL: `npm run tauri dev` → Analyze (first run downloads ~360MB models) → verify Detected facet/panel. (Optional: pre-stage models into app-data `models/` to skip download.)
- [ ] Pre-existing (not this feature): `core-pipeline` `tests/masks.rs` 1 clippy warning (concurrent masking work).

## Remaining work (prioritized)

> Full plan: `~/.claude/plans/act-as-senior-software-flickering-candle.md` (5 phases).
> Scope locked: pragmatic develop · personal macOS · full-DAM catalog · CR3-only.

### Phase 1 — Develop facade I — DONE ✅ (validated on real R7 CR3)

- [x] **Tone curve** → GPU: monotone-cubic LUT (`core-pipeline/src/curve.rs`) → 256×1 texture
      `@binding(3)`, sampled post-OETF/pre-contrast in `develop.wgsl`. Master + per-channel R/G/B.
      Controlled `ToneCurve.tsx`. Golden test `tests/tone_curve.rs`.
- [x] **HSL / color mixer** → GPU: 8 hue bands in `FxUniform` `@binding(4)`; display-space RGB↔HSV
      with normalized hue-band weighting in `develop.wgsl`. Controlled `ColorMixer.tsx` (global sat +
      per-hue H/S/L). Golden test `tests/hsl.rs`.
- [x] **Before/after** toggle: real `DEFAULT_PARAMS` render (store `showBefore`), `\` keybind +
      hold-to-preview + TopBar button. Removed the CSS-desaturate fake in `Stage.tsx`.
- [x] **Per-module Reset** via `resetKeys` (one render/persist) + tone-curve/color-mixer module resets.
- [x] **Histogram** from the real rendered buffer (`core-pipeline/src/histogram.rs` → `develop:histogram`
      event → `Histogram.tsx`). Replaced synthetic SVG.
- [x] **Search bar** wired (`TopBar` → app-store `onSearch` → `useLibrary.setSearch`, 300 ms debounce).

> Guard intact: `ParamsUniform` untouched — all new GPU data via new bindings. `param_effects` green.

### Phase 2 — Develop facade II

- [x] **Detail** (3×3 unsharp sharpen + luma/color NR) — wired single-pass via `@binding(9)`
      `ExtraUniform`; goldens in `param_effects.rs`.
- [x] **Lens vignette** — radial darken/brighten in the display stage (`@binding(9)`). Dead
      Profile/CA toggles removed.
- [ ] **Crop / geometry** (aspect + straighten angle) — still UI-only. Needs bilinear remap +
      export-aware output dims/overlay + interactive crop box + **visual QA**.
- [ ] **Lens distortion / chromatic-aberration** (manual k1 / per-channel radial) — still UI-only.
      Needs the same bilinear-remap infra + visual QA.

### Performance / robustness

- [x] Thumbnail cache **LRU eviction** — implemented (`core-library/src/thumbs.rs::evict_to`,
      size-bounded, wired at startup/post-index/post-import/cap-change).
- [ ] Dedicated **loupe preview** (≥1536px) instead of upscaled 512 thumb.
- [ ] Cache full-res developed buffer for repeat export; shorten `db`/`develop_cache` lock hold during decode/import.

### Library / catalog

- [x] **Filtering & sorting** — color-label filter (+ unlabeled sentinel), arbitrary star
      threshold, pick/reject, 8 sort orders (capture/filename/rating/imported); LeftNav
      All-photos/Picks/Recent wired. (`core-library/query.rs`, `BottomBar`, `LeftNav`)
- [x] **Keywords / tags** — full CRUD (`core-library/keywords.rs` + 5 commands), per-image editor
      (`RightInfo`) with autocomplete, batch tagging, LeftNav keyword filter, keyword-name search.
- [x] **Collections + smart collections** — static membership + smart predicate collections
      (`core-library/collections.rs` + 7 commands); LeftNav create/filter/delete; RightInfo
      membership editor; "save current filters as smart".
- [x] **Multi-select + batch ops** — cmd/shift-click selection, `SelectionBar` (batch
      rating/flag/label/keyword/collection/export), batch culling via keyboard, batch export.
- [x] **Import modes** — copy/move/reference picker (`ImportModal`).
- [x] FS **watcher** (`notify`) + reconciliation — implemented (`src-tauri/src/watch.rs` +
      `core-library/src/reconcile.rs`, real SQL status flips). Watch-out: can contend the DB lock /
      re-process files during an app import (gate it — see review).
- [ ] Keyword **hierarchy** (parent_id) UI; keyword rename/merge.
- [ ] "Recent import" as a true import-session filter (currently `imported_desc` sort).

### Packaging / security

- [ ] **Harden CSP** in `tauri.conf.json` (currently `null`): `img-src 'self' blob: data: thumb: http://thumb.localhost`, scope script/style/connect; test dev + prod.
- [ ] Tighten capabilities (least-privilege fs read/write scopes).
- [ ] Developer-ID **codesign + notarize** (set `APPLE_*` env); universal/x86_64 build if needed.

### Decode coverage

- [ ] Validate Sony `.ARW` / Nikon `.NEF` (latent via rawler, untested); LibRaw fallback feature (`libraw`), off by default.

## Watch-outs for whoever continues (see CURRENT_STATE.md for detail)

- Do NOT "fix" the `vec3 wb_gain` uniform alignment — it's correct; guarded by `param_effects` golden test.
- Develop works in **linear ProPhoto** now (`core-raw::map_3ch_to_rgb`); the shader converts
  ProPhoto→sRGB at the display transition (`PP_TO_SRGB`, derived in
  `core-raw/examples/print_color_matrices.rs`). Global WB is a **CAT mat3 on `@binding(8)`**
  (`params.rs::wb_matrix`, Planckian+Bradford, identity at temp=0); `ParamsUniform.wb_gain` stays
  identity. Detail/vignette = `ExtraUniform` on `@binding(9)`. Next free GPU binding = 10.
- Keep ALL rawler calls in `core-raw` (pinned `=0.7.2`, non-SemVer).
- `rusqlite 0.39` / `rusqlite_migration =2.5.0` pinned for rustc 1.91 — don't bump without checking MSRV.
- wgpu is `=29`; its API differs a lot from older majors (see CURRENT_STATE.md).
