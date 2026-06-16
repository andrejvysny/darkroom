# Darkroom — TODO

> Continuation tracker. Full status + architecture + gotchas in `CURRENT_STATE.md`.
> Plan: `~/.claude/plans/act-as-senior-software-piped-meteor.md`. Spec: `SPEC_V1.md`.

## V1 — DONE ✅ (all 5 acceptance criteria met + validated on real R7 CR3)

- [x] **Phase 0** Workspace, decode gate (8/8), `core-db` (DDL+migrations), app shell, Tauri wiring, dmg config
- [x] **Phase 1** `core-raw` + `core-library` indexing/thumbnails/queries, `thumb://`, Library UI (240/240 in ~2s; live render ✓)
- [x] **Phase 2** wgpu/Metal develop pipeline (WB/exposure/contrast/highlights/shadows/saturation/blacks/whites), ~2 ms/slider, edits persisted, Develop UI
- [x] **Phase 3** culling (rating/flag/label + keyboard loop), ⌘K palette + shortcuts, loupe zoom/pan
- [x] **Phase 4** `core-import` (copy/move/reference, date-routed, hash-verified) + `core-dedup` (byte+capture, Trash resolve) + UI
- [x] **Phase 5** export PNG/JPEG (full-res GPU) + dialog + ⌘E
- [x] **Phase 6** release `.dmg` (ad-hoc signed) — `Darkroom_0.1.0_aarch64.dmg` (checksum VALID)

Quality: `cargo test --workspace` (7 integration + unit, all green) · `cargo clippy --workspace` clean · `npm run build` clean.

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

### Phase 2 — Develop facade II (next)

- [ ] **Crop / geometry** (UV remap in new sampling stage; export-aware output dims). UI-only today.
- [ ] **Lens corrections** (manual distortion/CA/vignette in sampling stage). UI-only today.
- [ ] **Detail** (sharpen + luma/chroma NR; needs texel-size binding). UI-only today.

### Performance / robustness

- [ ] Thumbnail cache **LRU eviction** (`core-library/src/thumbs.rs` — currently unbounded).
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
- [ ] FS **watcher** (`notify`) + move reconciliation by content hash (spec §9).
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
- Keep ALL rawler calls in `core-raw` (pinned `=0.7.2`, non-SemVer).
- `rusqlite 0.39` / `rusqlite_migration =2.5.0` pinned for rustc 1.91 — don't bump without checking MSRV.
- wgpu is `=29`; its API differs a lot from older majors (see CURRENT_STATE.md).
