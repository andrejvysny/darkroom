# QA Campaign — Phase 0 (Stabilize & Ship)

> Consolidated in-app QA backlog from `TODO.md` + session notes. Run on the dev Mac with the real
> R7 library (`npm run tauri dev` unless noted). Mark each ✅/❌ + date; file findings as new TODO
> items. Roadmap: `~/.claude/plans/act-as-senior-software-misty-quasar.md`.

## A. Develop look & modules (subjective — real CR3s, varied scenes)

- [ ] ACR default brightness on varied real CR3 (portrait/landscape/backlit). Knob: `BASELINE_GAIN` (`core-pipeline/src/params.rs`, default 1.0).
- [ ] Color balance panel · crop/straighten · Temp/Tint · Sharpen/NR · Vignette respond and look right.
- [ ] Lens distortion/CA sliders on real wide-angle shots. Knobs: `LENS_K1/K2/CA_SCALE` (`params.rs`).
- [ ] Presence clarity/texture/dehaze look. Knobs: `LC_K_*` (`develop.wgsl`).
- [ ] Channel mixer + swap presets visual check.
- [ ] Crop: rot90, straighten-visible-in-crop, export dims correct.
- [ ] Tone curve editor: click-add / drag / dblclick-remove WYSIWYG vs render.
- [ ] Border/frame: pad-to-aspect + color in live preview AND export.
- [ ] AI denoise on a high-ISO 32MP R7 frame: quality + cache-swap doesn't glitch preview.
- [ ] AI masking (SAM): click-select object, mask edits apply, overlay correct.
- [ ] Zoom/pan/reset in Develop Stage + Library Loupe (post-`useViewport` extraction, no regression); histogram correct while zoomed, live on drag.

## B. Presets + History (`feat/presets-history`, merged)

- [ ] Apply preset to a 2nd already-edited image → untouched modules stay intact.
- [ ] Amount-blend look; hover live-preview snappiness.
- [ ] Copy/paste settings (⌘⇧C/V); ⌘Z/⌘⇧Z burst-coalesced.
- [ ] Snapshot create → restore **survives app restart**.
- [ ] Import a REAL Lightroom `.xmp` AND `.lrtemplate` → ImportReport shows WB-temp + color-grade as **dropped** (not silently applied).

## C. AI scan (unified modal, 0965d64)

- [ ] One scan runs detection+faces+captions; one progress bar; People populate before captions.
- [ ] **Stop mid-run**: durable partial results, no error rows clobbering good payloads; Stop during model download actually stops.
- [ ] Confirmed/assigned face survives a re-scan; `faces_delete_all` during scan refused.
- [ ] Scoped scan: Analyze on current view only; chevron whole-library escape hatch.
- [ ] Model manager: download progress, cancel, remove, retry; Settings accelerator readout = CoreML.

## D. Import / library

- [ ] RAW+JPEG pairing prompt on a mixed card; paired grid cell + `+JPG` badge; Unpair; "Show paired JPEGs"; dedup excludes companions.
- [ ] Delete rejected: LeftNav filter → two-step confirm → files in Trash, companions ride along.
- [ ] Import modes: Reference default; **reorder so Move isn't the middle click-target** (small code fix, do before QA).
- [ ] Move+Add: no Finder flash/sound/lag regression.
- [ ] Staged import: scan → dialog → dedup marks → commit; watcher picks external changes.

## E. HDR + Panorama (real captures)

- [ ] HIF opens in Develop ≈ CR3 sibling brightness; all modules respond; 33MP latency OK.
- [ ] Hand-held HDR bracket: frames register (warn+unaligned fallback OK); moving-subject deghost. Knobs: `DeghostParams{sigma,k}` (`core-hdr/src/lib.rs`). Stop on pill works.
- [ ] HDR merge → `_HDR.exr` row + chip → Highlights recovers headroom → export.
- [ ] Pano: stitch real sweep (streamed path) — seams/gain/boundary-warp on real parallax; Stop mid-merge leaves no partial `.dng`; result DNG develops with raw-like WB.
- [ ] Pano detect on full library: dismiss/undo persists across restart; merge handoff removes group; incremental re-run scans only new clusters; burst false-positive rate tolerable.
- [ ] Fixtures wanted: portrait HIF · clipped-highlight bracket · simultaneous RAW+HIF pair (refine 300-nit anchor).

## F. Stability / ship

- [ ] **SIGABRT soak**: scan → browse → develop ≥30 min; if it reproduces, grab crash report symbols.
- [ ] CSP (scoped, was null): thumbs + canvas render in `tauri dev` AND in built .dmg bundle.
- [ ] `.dmg` end-to-end: `bash scripts/macos-bundle-dylibs.sh stage` → `npm run tauri build -- --bundles dmg`; `otool -L …/MacOS/darkroom | grep heif` shows `@executable_path/../Frameworks/`; app decodes HIF on a Mac WITHOUT Homebrew libheif.
- [ ] Signed release: set `APPLE_SIGNING_IDENTITY` + one notarization secret set → `beta-*` tag → `spctl -a -vv` passes on a clean Mac → drop "isn't notarized" from README.
- [ ] Auto-update: prompt → download → restart from a previous installed version against the new release.
- [ ] Catalog backup (new): backup file appears in app-data `backups/`, rotation keeps N, "Back up now" works.
