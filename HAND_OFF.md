# Darkroom — Hand-off

> Read order when resuming: this file → `CURRENT_STATE.md` (architecture, gotchas, IPC surface) →
> `TODO.md` (granular leftovers) → `SPEC_V1.md` (full spec) → `CLAUDE.md` (hard constraints).
> Memory index: `~/.claude/.../memory/MEMORY.md` (latest: **darkroom-presets-history**,
> **darkroom-unified-ai-pipeline**, **darkroom-acr-curve-colorbalance**, **darkroom-tone-crop**).

## Repo state sync (2026-06-26)

`main` = **`f7445df`**, `origin/main` = **`1cbb3e3` (v0.1.1)**, **2 unpushed** (`e880fda` GPU hardening,
`f7445df` progressive preview). Since this TL;DR was last accurate, `main` also gained: Windows
packaging (NSIS + DirectML + Windows CI), the **dedup redesign** (UI + similarity-pipeline tightening),
**diagnostic logging**, Intel-macOS/beta CI, and the **v0.1.1 release**. `feat/presets-history` is
**100% uncommitted** on `f7445df` (18 modified + 14 new); its **hardening pass is DONE + headless-green
(2026-06-26)** — only in-app QA + commit remain. The "origin/main at f663ee0 / cleanup is the only
unpushed work" statements below are stale. **Consolidated in-app QA checklist: end of this file.**

## TL;DR

**Latest — branch `feat/presets-history` (built + verified headless & Tier-1, NOT committed/merged):**
Develop **Presets + edit-History + Lightroom preset import**. New **`core-preset`** crate (pure CPU, no
wgpu): a `serde_json::Value`-level **sparse merge engine** (`apply_sparse` + amount-blend), a `ModuleScope`
group→field map (Rust source of truth; `src/lib/presetScope.ts` mirrors it; drift-guarded), and a
format-agnostic import `Registry` (`PresetImporter` trait) with **Lightroom `.xmp`** (roxmltree, `crs:` ns)

- **`.lrtemplate`** (minimal Lua parser) → a `PresetIr` → an honest `ImportReport{mapped,approximated,
dropped}`. **Sparse per-field** presets (store only touched top-level `DevelopParams` fields, so applying
  never resets `toneAmount=100`/existing masks; typed round-trip in `src-tauri` keeps wgpu out of the parser).
  **LR fidelity best-effort:** absolute WB Kelvin + color-grade/split-tone **dropped** (no anchor /
  incompatible `cb_rgb` gain-power channels), basic-tone **approx**, HSL 1:1, curve `/255`. **Hybrid history:**
  in-memory ⌘Z/⌘⇧Z undo (burst-coalesced) + persistent DB **snapshots**. Migrations `015_presets` +
  `016_develop_snapshots`; 5 bundled built-ins; new left `DevelopSidePanel` (Presets | History) + create
  dialog + import-report modal + hover live-preview + copy/paste. All headless gates green + Tier-1 mock UI
  QA passed (0 console errors). **Pending: in-app GPU/CR3 QA + commit; then harden + extend** (`TODO.md` top:
  verify/harden/extend). Plan: `~/.claude/plans/act-as-senior-software-purrfect-glade.md`; memory
  `darkroom-presets-history`. **Adds no GPU bindings** (next free still = 15).

**Previously — branch `chore/cleanups-viewport-histogram` (MERGED to `main` `01a7b84`, not pushed):** a
tech-debt pass. (1) Extracted the ~200 LOC of duplicated canvas-viewport logic from `Stage.tsx` +
`Library/Loupe.tsx` into a shared `src/lib/useViewport.ts` hook (+ `src/lib/canvasPaint.ts` `paintFrame`);
behavior-preserving (crop fit-lock via `transformViewState`, tiered preview/decode stays in Loupe).
(2) **Whole-crop histogram**: new `develop_histogram` IPC renders the full crop `{0,0,1,1}` at 384² and
histograms it (correct while zoomed) — `develop_render` no longer emits the viewport-biased histogram;
the frontend triggers it on param/before-after change + first warm render (skip-if-cold avoids a
duplicate decode on open). (3) These doc reconciliations. `npx tsc --noEmit` + `cargo test` (Metal
goldens byte-identical) + `clippy --workspace --examples -D warnings` + `npm run build` all clean;
in-app visual QA pending. Plan: `~/.claude/plans/do-thorough-analysis-of-velvety-hollerith.md`.

**Latest MERGED to `main`:** see "Repo state sync" above — `main` is now `f7445df` (v0.1.1 + Windows
packaging + dedup redesign + logging on top of the AI/import work). `feat/unified-ai-pipeline`
(`f663ee0`) + `feat/import-ordering-keyset-paging` (`595685d`) are long since on `origin/main`.

- **`feat/unified-ai-pipeline` (MERGED `f663ee0`):** the two separate on-device AI passes (object
  detection auto-after-import + face recognition manual "Find People") are now ONE **manual** scan for
  **10k–100k libraries** — single shared decode (`core_raw::preview_with_orientation`), per-stage
  dirty-DAG keyset pagination (`stale_targets`), deferred Phase-B captions (Florence built lazily), and
  a **data-safe `reconcile_faces`** (a re-scan never drops a person-assigned face; inference errors retry
  instead of recording "0 faces"). No upstream person-gate — SCRFD self-gates ArcFace/clustering.
  Migrations `012` (`images(status,id)`) + `013` (`json_extract` marker cleanup); new `face_stage_enabled`
  IPC + Settings toggle; scan fully manual (auto-trigger removed); People ride the unified `analysis:*`
  stream. **Still pending: in-app GUI QA** + an optional independent Codex cross-check (not blocking).
  Design: memory `darkroom-unified-ai-pipeline`.
- **`feat/import-ordering-keyset-paging` (MERGED `595685d`):** capture-date ordering (file-mtime
  fallback, no stored NULLs), **keyset (cursor) pagination** for time-ordered sorts (migration `011`
  `idx_images_imported(status, imported_at, id)`; filename/rating keep OFFSET), client-side sorted-merge
  of incoming rows (kills live-import duplicates), and a throttled (500 ms) **live sidebar** (date tree +
  counts). ~500-line `useLibrary.ts` refactor. Design: memory `darkroom-library-tree-staged-import`.

**Prior MERGED pass (`d3e1d3e`): `feat/acr-curve-colorbalance` — base tone curve fit to the REAL Adobe
Camera Raw default + Color-balance-RGB.** Started from a 9-agent state audit that found the docs lagged
reality by two merged branches (crop/straighten, tone operator, import-lock fix, AI F1 0.905 were all
already done despite docs calling them open). Two features shipped:

- **Base tone curve fit to real ACR.** Replaced the placeholder analytic seed (`x^p/(x^p+c)`, p=1.35)
  with Adobe's **universal default tone curve** (1025-pt reference embedded in
  `core-pipeline/src/base_curve_ref.rs`, from RawTherapee `dcp.cc`). Verified via `exiftool` that the
  on-disk `Canon EOS R7 Adobe Standard.dcp` carries **no embedded ProfileToneCurve** → the R7 renders
  through exactly this universal curve. Per the user's "match ACR brightness" choice, mid-grey now maps
  **0.18 → 0.388 display-linear (≈65% sRGB)**, ~+1.3 EV brighter than before, so unedited imports match
  the Lightroom default. Codex/GPT-5.5-reviewed C¹ asymptotic highlight shoulder (the `1−k/(x+k)` tail
  can't pass through (1,1)). `BASELINE_GAIN` (`params.rs`, default 1.0, rides `ExtraUniform.texel.z`) is
  the one visual-QA brightness knob. `PROCESS_VERSION` 3→4.
- **Color-balance-RGB** (`@binding(14)` `CbRgbUniform`) — faithful subset of darktable `colorbalancergb`:
  4-way (global/shadows/midtones/highlights) + scene-linear contrast + saturation, in the
  **GPT-5.5-verified Filmlight grading RGB** (`grading_matrices`), with darktable's exact tonal opacity
  masks. **Identity at defaults → render byte-identical** (goldens unaffected). New `ColorBalance.tsx`
  panel. Quick win: eyedropper disarmed during crop mode.

Verified: `cargo test --workspace` (50+ tests) + `clippy --workspace --examples` + `npm run build`
all clean. Visual proofs: `cargo run -p core-pipeline --example {render_one,cb_demo}`. Details:
`CURRENT_STATE.md` → "Latest pass"; granular: `TODO.md` top section; plan:
`~/.claude/plans/act-as-senior-software-moonlit-zephyr.md`; deep notes:
memory `darkroom-acr-curve-colorbalance`.

**Next (what's left):** (1) **in-app visual QA** (`npm run tauri dev`) — confirm the brighter ACR
default + the Color balance panel + crop/straighten on varied real photos; tune `BASELINE_GAIN` if
the default look wants nudging. (2) **Develop fidelity continuation** (now unblocked): Lightroom
`.xmp` preset import, clarity/texture/dehaze (needs multi-scale blur), color-balance perceptual tail
(JzAzBz sat/brilliance, per-band, hue-shift, vibrance). (3) **Lens distortion / CA** (the only
UI-absent geometric module). (4) Viewport leftovers: whole-crop histogram, shared Stage/Loupe hook,
tiered source, B0 native-GPU-surface spike. See `TODO.md` for the full prioritized list. NOTE: local
photo data `2024/` (2.1 GB) + `Darktable_SORT/` (6.7 GB) are git-ignored — never commit them.

**Prior merged passes (all still current):** `feat/viewport-render` (full-res viewport zoom +
near-instant edits + mask overlay, `@binding(13)`); `feat/tone-operator-crop` (crop/straighten
`@binding(12)`, the scene-referred tone-operator infra `@binding(10/11)`); the **"Trust & Ship"
hardening pass** (per-image **sidecars** = rebuildable catalog, real histogram, BK-tree dedup, macOS
CI); V1 + post-V1 develop fidelity. Tone target is **Lightroom/ACR** (decided + now fit).

## How to run / build / test

```bash
npm install
npm run tauri dev                 # run app (first launch auto-indexes library/2026/)
npm run build                     # tsc + vite (frontend)
cargo test --workspace            # unit + integration (real CR3 + GPU where available)
cargo clippy --workspace --examples -- -D warnings
DARKROOM_REQUIRE_FIXTURES=1 cargo test --workspace   # what CI runs (missing fixture = hard fail)
npm run tauri build -- --bundles dmg                 # ad-hoc-signed dmg
```

App data: `~/Library/Application Support/com.andrejvysny.darkroom/` (`catalog.db` is WAL).
CI: `.github/workflows/ci.yml` (macOS runner — Metal + the committed CR3 fixture run for real).

## Current state — what's done

**V1 + post-V1 develop fidelity** (pre-existing): catalog/index/thumbnails; Library grid/nav/filter/
sort/keywords/collections/multi-select/batch; GPU develop (WB-as-CAT, exposure, contrast, highlights/
shadows, blacks/whites, saturation, tone curve, 8-band HSL, Detail sharpen/NR, vignette) in
linear-ProPhoto → sRGB; local masks (parametric/radial/brush/range); culling + ⌘K palette + loupe;
import (copy/move/reference) + dedup; export PNG/JPEG; AI scan (D-FINE-M + MegaDetector + MobileCLIP
verifier + Florence-2 caption); behavioral-signal capture.

**Trust & Ship (this pass) — all verified:**

- **Data-safety:** startup reaper for dangling import sessions; **atomic import copy**
  (`.part`→hash-verify→rename); removed `core-raw` panic surfaces (guarded `from_raw` + preview
  `adapt()` fallback); catalog stores **oriented** display W/H (portraits were recorded landscape;
  fingerprint still keyed on native dims via `Thumb.disp_*`).
- **Catalog integrity:** `Db::open` runs `quick_check` (typed `DbError::Corrupt`) + a schema-version
  **downgrade guard**; `wal_checkpoint(TRUNCATE)` on app exit. Migration `009` adds
  `idx_images_path` + `last_access` as **reserved** scale infra (see below).
- **Per-image sidecars** (`core-library/src/sidecar.rs`): `<raw>.CR3.json` (schema_version,
  content_hash, develop params, rating/flag/label, keywords). Written through on every edit/cull/
  keyword command; **hydrated on scan/import** for blank rows only (so in-app edits aren't clobbered);
  travels with copy/move imports. **Settings → "Write all sidecars" / "Rebuild from sidecars".**
  → Catalog is now a rebuildable cache: delete `catalog.db`, rescan, edits return.
- **Scale:** perceptual dedup O(n²) → **BK-tree** (`core-dedup`).
- **Honest UI:** real Library histogram (`image_histogram` → `core_pipeline::histogram_from_jpeg`,
  from the cached thumb); Stage zoom/pan resets on `selectedId` (not `imageUrl`, which broke zoomed
  editing); CommandPalette search now filters; dead Crop / Filmstrip-zoom / 1:1 controls removed;
  `selectedId` inits `null`; "Display P3" → "sRGB"; corrected stale `masks.rs` / `develop.rs` docs.
- **CI:** GitHub Actions on macOS — clippy `-D warnings`, `cargo test`, `npm run build`;
  `DARKROOM_REQUIRE_FIXTURES=1` makes a missing committed fixture fail (no silent pass).

New tests this pass: import-session reaper, schema downgrade guard, sidecar round-trip
(write→wipe→rebuild→restored + blank-only hydrate), BK-tree brute-force equivalence + identical-hash.

## Suggested next steps

### 0. Outstanding verification (do first)

0. **Presets/History in-app QA + commit** (newest, branch `feat/presets-history`, **uncommitted**;
   **hardening DONE 2026-06-26 + headless-green**) — run the consolidated QA checklist at the end of this
   file (`npm run tauri dev`), then **commit the branch**. Extensions remain in `TODO.md` top ("extend").
   Memory: `darkroom-presets-history`.
1. **Unified-AI in-app GUI QA** (`npm run tauri dev`) — `feat/unified-ai-pipeline` is MERGED but never
   GUI-verified: one scan does detection+faces+captions; ONE progress bar; People populate before
   captions; a confirmed/assigned face survives a re-scan; cancel works; `faces_delete_all` during a
   scan is refused. (First run downloads ≈900 MB object + 190 MB face models.)
2. **Cleanup-pass visual QA** (`chore/cleanups-viewport-histogram`, merged `01a7b84`) — zoom/pan/reset
   in Develop Stage + Library Loupe (the `useViewport` extraction must not regress); whole-crop
   histogram stays correct while zoomed + updates on slider drag. Then decide on **push** (this cleanup
   is the only work ahead of `origin/main`).
3. Deferred AI (optional): full Phase-A/B `run_pass` fn-split (cosmetic); ANN clustering
   (instant-distance HNSW) for >~200 k faces; drop the now-dead `analyze: bool` param in
   `index_root_blocking`. Optional independent Codex cross-check of the AI pass. Granular: `TODO.md`.

### A. Feature push — Develop fidelity (highest collaborator value; tone target = LR/ACR)

The input path is LR-grade. Items 1–2 are now **DONE** (`feat/acr-curve-colorbalance`); 3–4 remain:

1. ✅ **Scene-referred tone operator (keystone) — DONE & FIT TO REAL ACR.** Replaced the old `exp()`
   shoulder, then (this pass) replaced the analytic seed with Adobe's universal default tone curve
   (`base_curve_ref.rs`). `@binding(10/11)`. Applied once after all scene-linear edits. mid-grey
   0.18→0.388 (ACR brightness). Goldens `param_effects::base_curve_tone_response` + `acr_fit_tests`.
2. ✅ **Darktable color-balance-RGB — DONE (faithful subset).** 4-way + scene-linear contrast/sat in
   the Filmlight grading RGB, `@binding(14)`. **Deferred tail:** JzAzBz perceptual saturation +
   brilliance (needs PQ EOTF), per-band sat/brilliance, hue-shift, vibrance, gamut LUT.
3. ✅ **Lightroom `.xmp` preset import — DONE** (`feat/presets-history`, uncommitted): `core-preset` crate
   maps `crs:` keys → `DevelopParams` via a sparse IR (also `.lrtemplate`). Honest `ImportReport` — absolute
   WB Kelvin + color-grade/split-tone are **dropped** (no anchor / incompatible `cb_rgb` channels), not the
   "~70% incl. WB/color-grading" the earlier note assumed. Plus full preset CRUD + edit-history. **Verify +
   commit, then harden/extend** per `TODO.md` top. Remaining LR-adjacent extensions there: `.pp3`/`.costyle`
   importers, `.cube`/HaldCLUT (needs a new GPU 3D-LUT stage first), LR local-adjustment→masks import.
4. **Local-contrast family** (clarity/texture, then dehaze) — needs a multi-radius blur beyond the
   current 3×3. **Grain**, **channel mixer** (3×3 linear), **HaldCLUT/.cube** (3D texture, trilinear)
   are smaller independent wins. (NOTE: the earlier "saturation/HSL clamps away ProPhoto headroom"
   claim was **inaccurate** — global saturation already runs unclamped in scene-linear ProPhoto
   (`develop.wgsl::apply_local_linear`); the HSL clamp is correct display-space sRGB, post-OETF.)

### B. Geometry & crop (Marek's "most-missed in LR")

**Crop/straighten is DONE** (`feat/tone-operator-crop`): `sample_bilinear` 4-tap + `crop_to_source`
UV-remap (`@binding(12)`), interactive `CropOverlay.tsx`, aspect presets + straighten slider, export
at true dims (`Crop::export_rect`), UV threaded through mask/vignette coords. **Only remaining geometry
work: lens distortion / chromatic-aberration** (greenfield — reuse `sample_bilinear` for a radial UV /
per-channel scale on a fresh binding; then visual QA).

### C. Scale items — only when libraries actually grow (~200k+)

Deferred this pass because they're premature at 10k–50k on macOS (see memory for full rationale):
**FTS5** (LIKE is fast at 50k; FTS regresses substring→token), **keyset pagination** (offset-on-index
is fine for incremental scroll), **path-lookup rewrite** (HashSet is fine/faster at 50k), **DB-tracked
thumb LRU** (macOS APFS keeps atime). Migration 009's `idx_images_path` + `last_access` are already in
place for when these land.

### D. AI subsystem (own session)

Florence-2 KV-cache (O(n) decode), enable/calibrate the PresenceProbe (currently runs CLIP per image
while disabled in fusion), gate the wasted compute. Add an F1-0.905 regression guard (needs the
~900 MB models, so not in default CI).

### E. Pre-distribution (only if shipping beyond this Mac)

CSP hardening, Rust path-allowlist for export/import/index, Developer-ID codesign + notarize,
multi-format (ARW/NEF/DNG/Fuji) validation. All deferred while personal/macOS-only.

## Open items / gotchas

- **Verify in-app (not yet done):** edit/rate/keyword an image → confirm `<raw>.CR3.json` appears →
  quit (WAL checkpoints) → delete `catalog.db*` → relaunch/rescan → edits restored. Also visual-QA the
  real Library histogram + Stage zoom-during-edit.
- **Sidecar conflict policy** is "hydrate blank rows only" (catalog authoritative for non-blank). A
  newer sidecar from another machine won't override an already-catalogued row except via the explicit
  "Rebuild from sidecars" button. Tune if cross-machine sync becomes a real workflow.
- **Sidecars aren't removed** when a RAW is trashed (dedup resolve / manual) — harmless orphan `.json`
  next to a trashed file. Reconcile could GC these later.
- **Deferred, low-risk:** FK `ON DELETE` on `images.folder_id`/`import_session_id` (would need a table
  recreate — skipped as risky/low-value for single-connection use); de-`#[ignore]` the real-Trash
  import test (needs an injectable trash backend).
- **Hard constraints unchanged** — do NOT touch `ParamsUniform`/`wb_gain` packing; new GPU data on new
  bindings. Bindings 0–14 are now all wired (10 ToneOp, 11 base_lut, 12 Geom, 13 View, 14 CbRgb);
  **next free = 15**. rawler `=0.7.2`, wgpu `=29`, rusqlite `0.39`/`_migration =2.5.0` pinned for rustc
  1.91. See `CLAUDE.md`.

## Consolidated in-app QA checklist (run once via `npm run tauri dev`)

> These need a real GPU (Metal) + real CR3 and CANNOT be done headless — that's why they've stacked up.
> One app session burns down several merged-but-unverified branches at once. Tick each; note any
> regression. Headless gates are all green as of 2026-06-26.

**A. Presets / History / LR import** (branch `feat/presets-history`, uncommitted — verify THEN commit):

- [ ] Apply a preset to a **2nd, already-edited** image → only the preset's modules change; the prior
      edit's untouched modules (exposure, masks, crop, …) survive intact.
- [ ] **Amount** slider blends the look 0→100; the amount also applies on paste/snapshot where wired.
- [ ] **Hover** a preset row → live preview; mouse-out restores; hovering during a slider drag never
      sticks; switching image mid-hover doesn't restore the old photo's state (drag/hover guards).
- [ ] **Copy/paste settings** ⌘⇧C / ⌘⇧V; **undo/redo** ⌘Z / ⌘⇧Z (a slider drag = one undo step).
- [ ] **Snapshot** create → restore → **survives an app restart** (persisted in DB).
- [ ] **Import a REAL Lightroom `.xmp` AND `.lrtemplate`** → ImportReport shows WB-Kelvin + color-grade
      as **DROPPED** (not silently applied); basic-tone as approximated; HSL/tone-curve mapped. Confirm
      an element-form `.xmp` and a `.lrtemplate` with comments both import. A bogus/oversized/garbage
      file is rejected with a clear error (write-time validation + 4 MB cap).
- [ ] **Save** the current edit as a preset (module checklist) → reappears grouped, ★ toggles, export
      round-trips.

**B. Merged-but-unverified GPU work** (opportunistic — all already on `main`):

- [ ] **ACR brightness + Color-balance-RGB** look on varied real CR3 (the doc-flagged #1). Tune
      `BASELINE_GAIN` (`params.rs`) if the default is too bright/dark. (`feat/acr-curve-colorbalance`.)
- [ ] **Crop / straighten** — interactive rect + angle; export at true dims. (`feat/tone-operator-crop`.)
- [ ] **Whole-crop histogram** stays correct while **zoomed** + updates live on slider drag.
      (`chore/cleanups-viewport-histogram`.)
- [ ] **Viewport render** — crisp full-res zoom; red **mask overlay** color over a real mask; snappy
      masked edits. (`feat/viewport-render`.)
- [ ] **Unified AI scan** — ONE scan does detection + faces + captions; ONE progress bar; People
      populate before captions; a confirmed/assigned face survives a re-scan; cancel works;
      `faces_delete_all` during a scan is refused. (~900 MB + 190 MB models on first run.)
      (`feat/unified-ai-pipeline`.)
- [ ] **Dedup redesign** + **diagnostic logging** + (if on Windows) **NSIS build** smoke-check.
