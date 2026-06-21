# Darkroom — Hand-off

> Read order when resuming: this file → `CURRENT_STATE.md` (architecture, gotchas, IPC surface) →
> `TODO.md` (granular leftovers) → `SPEC_V1.md` (full spec) → `CLAUDE.md` (hard constraints).
> Memory index: `~/.claude/.../memory/MEMORY.md` (see **darkroom-trust-and-ship**).

## TL;DR

V1 + post-V1 develop fidelity were already complete. The latest work is a **"Trust & Ship" hardening
pass** (merged from `feat/trust-and-ship`): the catalog is no longer a single point of total data
loss (per-image **sidecars** make it a rebuildable cache), the app stopped lying (real histogram,
working palette search, no dead controls), perceptual dedup is sub-quadratic (BK-tree), and there is
a **macOS CI** gate so "green" means something. Verified: `cargo clippy --workspace --examples
-D warnings` clean · `cargo test --workspace` 32 suites / 0 failures (GPU + real CR3 run) · `tsc`
clean.

The next push is **develop fidelity / new modules** (the collaborator's wishlist), now safe to build
on. Tone target is **Lightroom/ACR** (decided).

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

### A. Feature push — Develop fidelity (highest collaborator value; tone target = LR/ACR)

The input path is LR-grade but everything after exposure is ad-hoc. Do in order — each gates the next:

1. **Scene-referred tone operator (keystone).** Replace the fixed `exp()` shoulder
   (`develop.wgsl::highlight_rolloff`) with a configurable filmic/base-curve applied **once** after all
   scene-linear edits (global + masks), tuned to the ACR look so imported presets render right. Fixes
   the flat look + the ordering nuance where mask exposure outruns the only highlight protection. New
   `@binding(10)` uniform; goldens in `param_effects.rs`. _Good candidate for a Codex/GPT-5.5 curve +
   test-vector review before coding (per CLAUDE.md)._
2. **Darktable color-balance-RGB** (Marek's #1) — 4-way lift/gamma/gain + chroma/luma in a perceptual
   space, scene-linear stage, new binding. Purely additive; reuses the mask range-selection infra.
3. **Lightroom `.xmp` preset import** (Marek's ⚠️) — new `core-preset` crate mapping `crs:` keys →
   `DevelopParams` (~70% maps: exposure/WB-via-as-shot/contrast/tone-curve/HSL/sat). The sidecar JSON
   format can grow an XMP-`crs:` bridge here. Do **after** 1–2 so remapped tone/grading match ACR.
4. **Local-contrast family** (clarity/texture, then dehaze) — needs a multi-radius blur beyond the
   current 3×3. **Grain**, **channel mixer** (3×3 linear), **HaldCLUT/.cube** (3D texture, trilinear)
   are smaller independent wins. Fix saturation/HSL to be chroma-preserving in unclamped linear
   (currently clamps away the ProPhoto headroom).

### B. Geometry & crop (Marek's "most-missed in LR")

Land one `sample_bilinear` 4-tap helper (input is non-filterable `Rgba32Float`) → wire crop/straighten
as a UV-remap stage, then lens distortion/CA. The pre/post-orientation dim decouple (done this pass)
is the prerequisite. Re-add the Crop & geometry UI (currently a "coming soon" placeholder) + a Stage
crop overlay. Thread the UV transform through mask/vignette coords.

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
  bindings (next free = **10**); rawler `=0.7.2`, wgpu `=29`, rusqlite `0.39`/`_migration =2.5.0`
  pinned for rustc 1.91. See `CLAUDE.md`.
