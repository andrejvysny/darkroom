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
