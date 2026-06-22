# Darkroom — Current State (handoff)

> Snapshot for resuming in a new session. Pairs with `TODO.md` (what's next + leftovers), `README.md`
> (overview), `SPEC_V1.md` (full spec).

## TL;DR

**Newest work — branch `feat/unified-ai-pipeline`, UNMERGED / uncommitted:** the two separate on-device
AI passes — object detection (auto-after-import) + face recognition (manual "Find People") — are merged
into ONE manual scan for **10k–100k-image libraries** (single shared decode, per-stage dirty-DAG,
deferred captions, data-safe face reconcile). `cargo test --workspace` + `clippy` + `npx tsc --noEmit`
clean; **NOT committed** (working-tree changes) and **in-app GUI QA + an independent Codex cross-check
are still pending**. Details: "Latest work — Unified AI pipeline" below; design + rationale in memory
`darkroom-unified-ai-pipeline`; fix plan `~/.claude/plans/act-as-senior-ai-linear-tome.md`.

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

## Latest work — Unified AI pipeline (branch `feat/unified-ai-pipeline`, UNMERGED / uncommitted)

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
- **`Stage.tsx` and `Loupe.tsx` duplicate** the canvas/view-rect/single-flight logic (the shared
  `useViewport`/`CanvasViewer` were created then removed as unused) — extract a shared hook later.
- **Histogram is now viewport-biased** (computed from the visible region). TODO in `commands.rs`:
  a separate small whole-crop histogram pass on param change (Codex #9).
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

| Crate           | Role                                                                                                   | Key files                                            |
| --------------- | ------------------------------------------------------------------------------------------------------ | ---------------------------------------------------- |
| `core-db`       | SQLite catalog: full DDL (STRICT), migrations, pragmas. Re-exports `rusqlite`.                         | `src/lib.rs`, `migrations/001_init.sql`              |
| `core-raw`      | rawler decode, embedded thumb/preview, EXIF meta, BLAKE3 hash, capture fingerprint, **linear develop** | `src/{develop,meta,thumb,hash}.rs`                   |
| `core-library`  | indexing (rayon), thumb cache, queries, culling, edit persistence                                      | `src/{index,query,thumbs,cull,edits}.rs`             |
| `core-pipeline` | **wgpu/Metal develop pipeline** (WGSL, prepare/render), PNG/JPEG encode                                | `src/{backend,params,encode}.rs`, `src/develop.wgsl` |
| `core-import`   | copy/move/reference import, date routing, verify, Trash                                                | `src/lib.rs`                                         |
| `core-dedup`    | byte + capture grouping, safe resolve→Trash                                                            | `src/lib.rs`                                         |
| `src-tauri`     | IPC commands, `thumb://` protocol, managed state                                                       | `src/{commands,protocol,state,lib}.rs`               |

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
  `develop_get_histogram`, `image_histogram` (Library panel)
- Export: `export_image`
- Culling: `cull_set_rating`, `cull_set_flag`, `cull_set_label`,
  `cull_set_rating_many`, `cull_set_flag_many`, `cull_set_label_many` (batch)
- Keywords: `keywords_list`, `keywords_for_image`, `keyword_add_to_image`,
  `keyword_add_to_images` (batch), `keyword_remove_from_image`, `keyword_delete`
- Collections: `collections_list`, `collections_for_image`, `collection_create`,
  `collection_rename`, `collection_delete`, `collection_add_images`, `collection_remove_images`
- Dedup: `dedup_scan`, `dedup_resolve`
- Import: `import_start`
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
(`"__none__"` = unlabeled), `keyword_id`, `collection_id`, `import_session_id`, `search`
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
export at true dims via `Crop::export_rect`. **Still UI-absent / NOT wired:** Lens distortion /
chromatic-aberration only (greenfield — no shader math, no UI controls yet). **Bindings 0–14 all used;
next free = 15.**

**Not done (deferred from spec):** keyword hierarchy UI, "recent import" as a true session filter,
per-display ICC, RCD/AMaZE demosaic, Windows/Linux, notarization, CSP hardening. (Thumbnail LRU
eviction and FS-watcher reconciliation are DONE — see `thumbs.rs::evict_to` and `src-tauri/watch.rs`.)

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
