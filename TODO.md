# Darkroom — TODO

> Continuation tracker. Full status + architecture + gotchas in `CURRENT_STATE.md`. Spec: `SPEC_V1.md`.

## HDR/Pano review + remaining-work pass (2026-07-20, UNCOMMITTED on `main`) — CURRENT

> Plan: `~/.claude/plans/act-as-senior-software-elegant-flurry.md` (review verdict + 4 tracks).
> All four code tracks (A–D) are **done + headless-green**: `cargo test --workspace` 52 suites /
> 0 failures, `cargo clippy --workspace --examples`, `npx tsc --noEmit`, `npm run build` all clean.
> **What's left is QA that needs the dev Mac + real captures** — the checklist directly below.

### Real-corpus validation round (2026-07-20) — 2137 files, 38 GB, `~/Pictures/DCIM/100EOSR7`

Ran the whole stack against a real R7 card dump (1911 CR3 · 171 HIF · 52 JPG). **Two hard bugs
found that made large parts of the library unusable — both fixed, both now covered by tests.**

- [x] **ALL 171 real `.HIF` files were rejected** (`develop_linear` → "unsupported color profile").
      Root cause: a real Canon HIF's primary item is a **4×5 grid** (derived item), and
      `heif_image_handle_get_nclx_color_profile` reports `Unspecified/Unspecified` for it even
      though `heif-info` lists an nclx and every tile is BT.2020 PQ — libheif only surfaces nclx
      from a `colr` box on the item it was asked about. The committed synthetic fixtures are
      single-item, so this never showed up; the one "validated" real file regressed the same way on
      Homebrew libheif 1.21.1. Fix: `core-raw/src/heif.rs::container_nclx` parses the container's
      own `colr` boxes (`meta/iprp/ipco`) and treats them as authoritative, falling back to the
      handle only when the container carries none; **every** nclx found must agree on BT.2020 PQ.
      Now 171/171 decode; the Apple gain-map negative test still rejects (its `colr` is an ICC
      `prof`, not nclx). Guarded by 4 unit tests over hand-built ISOBMFF boxes (no fixture needed).
- [x] **557 of 1911 CR3s failed to index** — every Canon **HDR-PQ CR3** (`CanonCR3_003`, written
      whenever the body shoots HDR PQ). rawler cannot extract their embedded preview (it's HEVC,
      not JPEG) and returns a hard error, which the indexer treated as a failed file — even though
      **the mosaic beside it decodes perfectly**. Fix: `thumb.rs` folds preview-extraction errors
      into "no preview" and falls back to developing the RAW (`embedded_preview` +
      `developed_preview`), for the thumbnail path *and* the AI-scan preview paths. Full index is
      now **2134/2134, 0 failed**. Dimension care: the fallback reports true sensor dims (`src_*`
      feeds the capture fingerprint) and true oriented dims (`disp_*`), never the thumbnail's —
      a first cut got this wrong and reported 512×341. Regression test `tests/hdrpq_cr3.rs`
      (fixture-gated on `$DARKROOM_CR3_FIXTURES`).
- [x] **Panorama streaming recall regression (mine, from Track C)**: `downscale_native` used a
      **Triangle** filter, and at ~5× reduction the aliasing cost real feature matches — on a real
      14-frame sweep the streaming path put only **8 frames** in the largest component where
      full-res registration found 10. Switched the pano low-res pass to `downscale_into_hq`
      (Lanczos3): now **12 frames**, a wider composite (9660×3069 vs 8196×3003), and the faint
      vertical seam band is gone. Registration itself was never wrong (focal within 1.1%).
- [x] **Risk R4 (portrait `.HIF`) RESOLVED — no code change needed.** 82 of the 171 HIFs are
      portrait. `heif_gate` shows decoded dims differ with vs without container transforms
      (4640×6960 vs 6960×4640) → libheif applies `irot` itself, so `.oriented()` must NOT be added.
- [x] Detection on the real library: 2134 candidates → 460 clusters → **73 groups in 8.2 s**,
      identical headless and in-app. Spot-checked visually: genuine multi-frame sweeps are found;
      **~15 % (11/73) are continuous-drive bursts** (e.g. a bird tracked across the sky) that
      geometrically mimic a sweep because the camera panned. Capture rate separates them cleanly
      (bursts ≥2 fps, real sweeps ≤1.4 fps) — deliberately NOT gated on, because a missed pano is
      silent while a junk suggestion costs one dismissal click. Recorded as a tuning lever.
- [x] HDR merge on real ±3 EV brackets (the corpus has 141 such runs; note **no AEB** — all
      manual): 7 of 8 non-reference frames aligned at 99.7–100 % coverage, including a portrait
      bracket; the one failure (the +3 EV frame, blown out) fell back to unaligned with a warning
      exactly as designed. Merged output recovers headroom (max 1.33) with no visible ghosting.
- [x] New harnesses: `core-library --example detect_catalog` (headless mirror of the app's
      detection job over a real catalog) and `--example index_root` (reference-mode import);
      `stitch_cr3` now exercises the **streaming** `FrameSource` like the app does, instead of the
      old resident path.

**Open from this round:**

- [ ] **One unexplained crash** (`SIGABRT`, main thread, symbols unresolved) ~15 min after a
      detection scan. NOT reproduced in a clean 2-minute run, and the crashed process had been
      orphaned from its dev server by an earlier process kill, so it may be an artifact of that.
      Worth a deliberate long-running soak (scan → browse → develop) before trusting it.
- [ ] Import dialog now defaults to **Reference** (was Copy in code). Consider also reordering the
      options so the destructive **Move** isn't the middle click-target.
- [ ] Burst-vs-sweep precision (above) if the review queue feels noisy in practice.

### BLOCKING QA (needs the Mac / real files) — do this next

- [ ] **Panorama, real captures** (the long-standing blocking gate): `cargo run --release -p
      core-raw --example stitch_cr3 -- <dir-of-pano-CR3s> /tmp/o.dng`, then open the DNG in Develop
      (WB must behave raw-like). Judge seam/gain/boundary-warp quality on REAL parallax + exposure
      drift — everything so far is synthetic scenes. Then in-app: select → preview → merge → the
      pano appears linked + editable. **New this pass, so QA it deliberately:** the streaming path
      changed how frames reach the compositor (2 decodes/frame; seams/gains now estimated from
      1400 px buffers), so compare a streamed result against a pre-change stitch if anything looks
      off; also exercise Stop mid-merge (cancel is now honored inside `stitch`, not just between
      frames) and confirm no partial `.dng` is left behind.
- [ ] **Hand-held HDR** on a genuinely hand-held bracket (the validated set was tripod, so
      alignment was near-identity): confirm frames register (the merge warns per-frame when they
      don't, and falls back to unaligned rather than failing), and shoot something with a MOVING
      subject to judge deghosting — `DeghostParams { sigma 0.05, k 0.25 }` in `core-hdr/src/lib.rs`
      are the tuning knobs. Also exercise Stop on the HDR pill.
- [ ] **`.dmg` build end-to-end** — `npm run tauri build -- --bundles dmg`, then verify the app
      launches HEIF decode on a machine WITHOUT Homebrew libheif (that's the whole point of the
      bundling), e.g. `otool -L Darkroom.app/Contents/MacOS/darkroom | grep heif` shows
      `@executable_path/../Frameworks/...`, and Contents/Frameworks holds the 6 dylibs.
- [ ] **First Developer ID signed release** (`docs/macos-signing.md`): release.yml now signs +
      notarizes + staples on macOS whenever the secret set is complete, and hard-fails on a partial
      one. Add `APPLE_SIGNING_IDENTITY` plus ONE notarization set (API key `APPLE_API_KEY` /
      `APPLE_API_ISSUER` / `APPLE_API_KEY_BASE64`, or Apple ID `APPLE_ID` / `APPLE_PASSWORD` /
      `APPLE_TEAM_ID`), then cut a `beta-*` tag first and open the downloaded DMG on a Mac that has
      never built the app. Untested end-to-end: nothing in this repo has ever run a real
      notarization. Once it lands, drop the "isn't notarized" paragraph from README.md.
- [ ] **HIF in-app QA** (unchanged from the earlier list): HIF opens in Develop at ≈ the CR3
      sibling's brightness, all modules respond, thumbs/preview latency acceptable on 33 MP.
- [ ] Detection in-app: Detect from the new LeftNav button, dismiss/undo persistence across
      restart, merge handoff drops the group from review immediately, incremental re-run after new
      imports only scans new clusters.
- [ ] Fixtures still wanted: a PORTRAIT `.HIF` (risk R4 — decides whether `heif.rs` needs
      `.oriented()`), a bracket with CLIPPED highlights, and a plain RAW+HIF simultaneous pair to
      refine the 300-nit anchor.

### Landed this pass

- [x] **Track A — correctness fixes** (headless-green): A1 detect-merge attribution via IPC echo
      (`detectGroupId` rides `panorama_merge` → echoed on `panorama:done`; `activePanoDetectGroupId`
      store field DELETED); A2 `panorama_sources` links only `result.used_indices` + "stitched N of
      M" toast; A3 `hdr_merge` dedupes ids + refuses mixed camera bodies + SelectionBar 2–9/RAW
      gate; A4 pano failure cleanup (truncated/orphan DNG removed, preview cache cleared on failed
      merge, `canMerge` blocks on `previewError`); A5 docs reconciled.
- [x] **B-P6 — release plumbing**: `scripts/macos-bundle-dylibs.sh` (BFS libheif closure →
      version-stripped staging in `src-tauri/frameworks/` + install_name_tool rewrites; wired as
      `build.beforeBundleCommand`; tauri-bundler copies+signs but never rewrites names — verified),
      `bundle.macOS.frameworks` (6 stable names), CI `brew install libheif` (ci.yml macOS job +
      release.yml macOS legs; no Linux jobs exist). NOT yet exercised: a real
      `npm run tauri build -- --bundles dmg` on the dev Mac (QA item).
- [x] **Track B — hand-held HDR** (headless-green + validated on the real ±3 EV R7 bracket):
      shared `features::extract_at` (keypoints in TRUE full-res units regardless of buffer
      downscale; `extract` is now a wrapper — behavior-preserving, existing pano tests untouched);
      **`core_pano::align`** (`estimate_alignment_rgb`, nalgebra-free `[[f64;3];3]` out) with a new
      **affine RANSAC** (`ransac::verify_pair_affine`, 3-pt fit, `w³` adaptive exit, same
      SplitMix64 + Brown&Lowe gate) — affine by default because projective terms extrapolate
      wildly across textureless sky; **`core_hdr::warp_into_reference`** (bilinear inverse warp +
      validity mask, rayon rows); **`add_frame_masked`** (masked-out pixels SKIPPED — `hat_weight(0)
      = W_LOW_FLOOR = 0.05` would otherwise paint dark halos at warp borders) and
      **`with_reference` + `DeghostParams{sigma 0.05, k 0.25}`** (consistency weight
      `w *= 1 − ref_conf·(1−consist)`; deghost auto-disables where the reference clips so darker
      frames still fill highlights). Unregisterable frames merge unaligned + warn (never fail).
      `hdr_cancel` IPC + `AtomicBool` polled per frame + Stop button on the pill; `hdr:done` carries
      `warnings[]`. Harness: `cargo run --release -p core-hdr --example merge_one HDR` →
      both frames "aligned (99.9% valid)", EV math exact (×7.81/×0.125).
      Alignment accuracy on synthetic scenes: affine **0.048 px**, homography 0.043 px.
- [x] **B-P5 — interop**: migration **020 `hdr_sources`** (mirrors `panorama_sources`, cascade-
      tested) populated at merge; `core_library::merge_sources` + `image_sources` IPC + RightInfo
      **"Source frames"** section (HDR shows per-frame relative EV; missing sources dimmed). Rows
      are non-clickable by design — `selectedImage` is derived from the loaded grid page only, so
      click-to-select would silently no-op for off-page sources.
      **fp16-DNG spike verdict: rawler CAN write float DNG** — `RawImageData::Float` +
      `DngCompression::Uncompressed` (Lossless silently force-converts float→u16; commented in
      code). f32 only (no fp16 writer), no compression → **388 MB** for a 33 MP export. Shipped as
      `core_raw::write_hdr_dng` + `hdr_export_dng` IPC + RightInfo "Export DNG…" (hdr rows only).
      ColorMatrix1 = `XYZ_TO_PROPHOTO_D50` (buffer is ProPhoto, not camera-native), AsShotNeutral
      [1,1,1]. Verified on the real merged EXR: raw SubIFD reads `Float/32/Linear Raw/Uncompressed`
      (IFD0 "Unsigned" is the 8-bit preview — check with `exiftool -a -G1`). Harness:
      `cargo run --release -p core-raw --example export_hdr_dng -- <in.exr> [out.dng]`.
- [x] **Track C — pano streaming + cancel + reconnect** (headless-green): **`FrameSource`** trait
      (`load(i, max_long_side) -> LoadedFrame` carrying buffer dims **and** true full-res dims) with
      a blanket `impl for &[Frame]`; `stitch_streaming`/`register_streaming` alongside the
      UNCHANGED public `stitch`/`register`/`compose` (thin wrappers, resident path byte-identical —
      guarded by `slice_source_reproduces_the_resident_stitch_exactly`).
      Data-flow verified first: **no stage needs all full-res warps at once** — seams + gains
      consume only the ~1400 px `low_warps`, and the 5-band blend already streamed one frame's
      pyramids at a time. So: load all N at seam resolution → register via `extract_at` (poses in
      TRUE full-res units — the units trap that naive "register on low-res" hits) → seams/gains on
      lows → `release_low()` → blend loads full-res ONE frame at a time. **Peak frame RAM ~3.8 GB →
      ~0.55 GB** for a 10-frame 32 MP merge, at 2 decodes/frame. `UsedCam::scaled(ratio, w, h)`
      takes explicit dims (the source's downscaler owns rounding; a 1-px disagreement would let
      `warp` index past the buffer). Mixed-camera check moved into `CatalogFrameSource::load`
      (still fails fast — the low pass reads every frame before compositing).
      **Cancel** now threads `&(dyn Fn()->bool + Sync)` through load/feature/pair/warp/blend loops
      and finally constructs the long-dead `PanoError::Cancelled`; `panorama_status` gained an
      `ipc.ts` wrapper + a one-shot `usePanorama` reconnect probe so the pill returns after a
      renderer reload.
- [x] **Track D — detection hardening** (headless-green): detect state (running/suggested/groups/
      loading/progress) lifted into the zustand store with **module-singleton listeners** (mirrors
      `usePanorama.ts`) — kills the duplicate toasts + doubled IPC from the two always-live
      `usePanoDetect()` consumers, and keeps the LeftNav badge and the review overlay in sync after
      dismiss/merge. **`prune_stale_groups`** runs at scan start (brief DB lock): deletes
      `status='suggested'` groups with <2 members whose image ROW still exists — the unreachable
      husks left when dedup hard-deletes members (cascade drops the member rows, and
      `replace_cluster_groups` can only stale-delete groups intersecting a re-verified cluster).
      **Deliberately keyed on row existence, not `status='present'`**: a merely-missing member
      (unmounted volume) must not drop the group, because the per-image scan markers would then
      suppress re-verification and lose the suggestion until a forced rescan — those are surfaced
      instead via `PanoMemberRow.present` (dimmed in review, merge blocked with a distinct reason
      from the `allRaw` gate). LeftNav "Panoramas" gains a Detect/Re-detect button + running
      indicator (generalized `AnalyzeButton` → `RunButton`), so a scan is startable without opening
      the overlay.
      ~~Known layering wart~~ **FIXED**: `usePanoDetect` now listens to `panorama:done` itself and
      owns mark+refresh, so `ipc.ts` is pure transport again (no hook import, no import cycle) and
      `usePanorama` no longer knows about detection at all.

**Fixed en route (pre-existing):** `heif_decode::non_pq_heif_rejected_cleanly` asserted the wrong
rejection branch's wording — the Apple gain-map fixture reports *Unspecified* primaries, so it hits
the "only BT.2020 PQ … is supported" message, not "expected BT.2020 PQ". Rejection behavior was
always correct; the assertion now matches both branches. (Confirmed pre-existing by stashing.)

## HDR pass follow-ups (2026-07-18, branch `claude/hdr-heif-support-5r5ztx`) — CURRENT

Landed: Canon HDR PQ `.hif` full develop support (libheif → PQ → linear ProPhoto) + Merge-to-HDR
tripod v1 (fp16 linear-ProPhoto EXR, `hdr_merge` IPC, SelectionBar/palette UI). All Linux-runnable
gates green (incl. a committed synthetic 10-bit PQ HEIF fixture decoding byte-exact). Details:
`CURRENT_STATE.md` "Latest pass — HDR". **Binding-map correction: 0–15 all used (15 = ChanMix);
next free = @binding(16).**

**Sample-based validation round (2026-07-18, same branch — user has no R7 files yet, so public
GitHub samples + synthesis were used; downloads live UNCOMMITTED in `library/fixtures-samples/`,
already gitignored):**

- **R1 (libde265 vs Canon's HEVC Main 4:2:2 10 intra) retired at codec level:** a second committed
  fixture `synthetic_pq422.hif` (591 B, heif-enc `-p chroma=422`, verified `YCbCr 4:2:2 / 10-bit`
  via heif-info) decodes byte-exact through `develop_linear` — CI-guarded by
  `synthetic_pq422_heif_round_trips`. What this does NOT cover: Canon's real container (heix brand,
  grid/tiled layout, embedded thumb, irot) — still a Mac+fixture item.
- **Negative-profile test committed:** `apple_hdr_gainmap.hif` (real iPhone 13 Pro gain-map HEIC,
  MIT — `APPLE_HDR_LICENSE`) → clean "expected BT.2020 PQ" rejection, metadata still readable
  (`non_pq_heif_rejected_cleanly`).
- **Corpus survey:** all 51 Nokia `heif_conformance` stills through `heif_gate` (now
  per-file fault-tolerant): 35 decode OK, 16 fail with clean libheif errors (sequences /
  intentionally-broken items), zero crashes. All decodable ones are 8-bit — the suite has no
  10-bit stills, hence the synthesized 4:2:2 fixture above.
- **Merge plumbing validated on real sensor data:** fabricated ±2 EV bracket (3 copies of the
  committed `_55A3947.CR3`, `ExposureTime` rewritten via exiftool 12.76 — rawler parses the
  rewritten files fine) through `merge_one`: EV math exact, median reference picked, 3×32.3 MP
  reference-WB decodes, streaming merge, 107 MB EXR written + read back via the thumbnail path.
  NOTE: frames share identical pixels, so output brightness is a scale-blend (~×1.75 midtones) —
  plumbing proof only, NOT an HDR-look check. Recreate with:
  `cp` ×3 + `exiftool -overwrite_original -ExposureTime=… frame_m2ev.CR3` (one file per invocation)
  + `cargo run -p core-hdr --example merge_one library/fixtures-samples/bracket`.

**Real-file validation round (2026-07-19, DONE here on Linux with the user's real R7 HDR shot —
`855A6554.HIF` + 3 bracketed CR3s `_55A6551–3.CR3`, ±3 EV, staged from the repo's `HDR/` folder on
`main` into `library/fixtures-hdr/`):**

- [x] S1 real-file gate PASSED: real R7 .HIF (6960×4640, 10-bit, nclx BT.2020/PQ/full-range)
      decodes via libde265; Exif parses (model/ISO/shutter); no irot on landscape files; carries
      embedded 10-bit PQ thumbnails (320×214 + 1620×1080). → Implemented: embedded-thumbnail fast
      path for thumb/preview duty (~10× faster grid thumbs, primary still used when the requested
      edge exceeds the thumb) + rayon-parallel PQ→ProPhoto conversion.
- [x] S2 calibration DONE: metered CR3 vs camera HIF measured **+0.572 EV** → anchor adopted
      **HDR_DIFFUSE_WHITE_NITS = 300** (measurement + composite-source caveat recorded in
      color.rs; refine against a plain RAW+HIF simultaneous-recording pair if one appears).
- [x] merge_one on the real bracket: EV math exact (×7.81/×0.125), no visible ghosting vs the
      camera HIF (geometry identical), merged EXR + SDR preview produced. Note: this scene never
      clips the metered frame (f/22 sunset, max ≈275 nits), so it exercises shadow-noise merging,
      not highlight recovery.

**Still needs the dev Mac (or more fixtures):**

- [ ] PORTRAIT .HIF check (no portrait fixture yet): if with/without-transform dims match but the
      EXIF tag says 6/8, add `.oriented()` in `heif.rs` (risk R4).
- [ ] A bracket with CLIPPED highlights to visually confirm highlight recovery (this set had none).
- [ ] A plain RAW+HIF simultaneous-recording pair to refine the 300-nit anchor (current pair is an
      HDR-mode composite, which may bias mid-tones).
- [ ] In-app QA (`npm run tauri dev`): HIF opens in Develop (side-by-side vs CR3 sibling ≈ same
      brightness under defaults); WB/exposure/all modules respond; thumbs + `develop_preview_jpeg`
      latency acceptable on 33 MP HIF (full decode, no embedded preview fast path yet); select
      bracket → Merge to HDR → pill → new `_HDR.exr` row with HDR chip → develop (Highlights slider
      recovers headroom) → export JPEG; HEIF/HDR filter chips; dedup scan on the CR3+HIF pair
      (same-capture groups like RAW+JPEG — expected, manual resolve only).
- [ ] Release checklist: bundle `libheif.1.dylib` + `libde265.0.dylib` (otool -L closure) via
      `tauri.conf.json` `bundle.macOS.frameworks`; CI macOS job needs `brew install libheif`,
      Linux jobs `apt-get install libheif-dev libde265-dev`.

**Deferred increments (do NOT creep into this pass):** alignment (MTB) + deghosting + auto
bracket detection; hand-held merge; fp16 DNG export (Lightroom interop); HEIF *export*; general
`.heic`/iPhone (non-PQ profiles get a clean error today); Windows HIF decode (vcpkg libheif — cfg
stub ships); `hdr_sources` DB table + "show source frames" UI (parentage lives in the EXR's
`darkroom:sources` attr); HDR/EDR display output.

**Known dedup interaction:** the merged `_HDR.exr` shares the reference CR3's `capture_fingerprint`
(same model/date/shutter-count/dims), so it surfaces as a same-capture dedup group with its own
source frame — expected, manual resolve only, same class as RAW+HIF pairs (see the in-app QA item
above).

## IN PROGRESS: Panorama merge (branch `claude/darkroom-panorama-research-ruvw38`, 2026-07-18)

P0–P4 landed + headless-green (see `CURRENT_STATE.md` top section for the full map). Remaining:

1. **Visual QA on real panos (BLOCKING before merge to main)** — needs the dev machine's CR3 sets:
   `cargo run --release -p core-raw --example stitch_cr3 -- <dir> /tmp/o.dng`, eyeball the proof
   JPEG + open the DNG in Develop (WB slider must behave raw-like; check seam quality on real
   parallax/exposure drift), ideally `dng_validate` from Adobe DNG SDK. Then in-app: select →
   merge dialog → preview → merge → pano appears linked + editable.
2. ~~P5 Boundary Warp~~ **DONE** — `core-pano/src/rectangle.rs`: inverse bilinear mesh warp
   (boundary attraction + membrane shape-preservation, CG solve, distance-gated interior anchor so
   the warp has compact support, global-factor bisection guarantees no quad folds). Synthetic:
   crop area ×1.179 at warp=100, interior correlation 1.0000, warp=0 byte-identical. Line
   preservation deliberately out of scope v1.
3. ~~P5 seam/ghost quality~~ **DONE (with a measured verdict)** — graph-cut seam implemented
   (`pathfinding` Edmonds-Karp, ≤150k nodes) but **DP stays default**: benchmark showed ~35 ms
   (DP) vs ~42 s (graph-cut) at identical seam quality on the synthetic scenes; kept behind the
   internal `SEAM_METHOD` const with an `#[ignore]`d benchmark test. Deghosting: gain-corrected
   diff + 8×median ghost mask (3px dilated) as hard seam penalty — moving objects render from a
   single source. Re-benchmark graph-cut on real parallax captures before reconsidering.
4. **Memory follow-up** — streaming decode-on-demand `Frame` source so `merge` never holds all
   full-res sources (~4 GB at 10 R7 frames today); band-tiled compositing if gigapixel ever matters
   (cap is 12000px now so the develop pipeline can open the result).
5. **Small leftovers** — ~~free the preview cache on modal close~~ **DONE 2026-07-19**
   (`panorama_preview_release` command, called from PanoramaModal's close cleanup);
   `panorama_status` has no frontend consumer yet (reconnect-after-restart UX); HDR-pano
   (bracketed) explicitly out of scope for v1.

## IN PROGRESS: Panorama detection (branch `claude/panorama-detection-ob1jjq`, 2026-07-19)

"Detect panoramas" landed end-to-end — migration 019, `core_pano::detect_groups`,
`core_library::pano_detect`, `src-tauri/pano_detect.rs` job + 6 commands, `PanoSuggestions` review
UI with merge handoff — and is **validated on real photos** (4/4 ground-truth groups, 0 false pairs;
see `CURRENT_STATE.md` top section). Remaining:

1. **In-app QA on the dev machine's real CR3 library** — run Detect from the LeftNav Panoramas
   section; confirm real sweeps group (typical pano: 2–10 frames seconds apart at fixed focal),
   dismiss/undo persistence across restarts, merge handoff → `pano_detect_mark_merged`, and that an
   incremental re-run after new imports only scans the new clusters. (Container validation used
   JPEG corpora + mocked UI; the thumb-based detection path is format-agnostic.)
2. **Threshold audit on messier corpora** — hand-held multi-row panos, moving subjects, zoom drift.
   Knobs: `core_pano::DetectOptions`, `core_library::pano_detect::ClusterParams`. Bump
   `ALGO_VERSION` on ANY change so the incremental scan invalidates.
3. Optional: surface Burst edges as a "burst stack" suggestion category (already classified in
   `DetectReport.edges`, currently not persisted).
4. Tier-3 e2e (real backend) of detect → review → merge, macOS.

## Repo state sync (2026-07-02) — CURRENT

- **`main` = `8c1072c`** (unpushed to origin), version **`0.2.0` (beta-2.0) RELEASED**. Since the
  2026-06-26 note below: `feat/presets-history` (MERGED `c8e37bd`), `feat/windows-hardening`
  (`4fd90dc`), `feat/jpeg-png-support` (`b283caf`), release `0.2.0` (`99dc448`), and the develop-render
  overhaul `8c1072c` (blank-canvas + zoom-distortion fixes; memory `darkroom-render-pipeline-fixes`) are
  all on `main`. Real-backend e2e = **39/39** green (`e2e/VERIFICATION.md`); the old fake-histogram
  finding is fixed. Bindings 0–14 used; next free = **@binding(15)**. PROCESS_VERSION = 4.
- **`feat/lens-corrections` MERGED to `main` (`5eb2d22`, --no-ff, UNPUSHED)** — the "absent develop
  modules" plan (`~/.claude/plans/do-thorouhg-analysis-of-synthetic-yao.md`), headless-green. DONE:
  - **Lens distortion (k1/k2) + lateral CA** — extends `GeomUniform` @12 (`lens:[f32;4]` + `aspect.z`
    active flag; byte-identical at defaults, so **no PV bump**), new `lens_sample` in `develop.wgsl`,
    `dist_k1/dist_k2/ca_red/ca_blue` on `DevelopParams` + 4 sliders in the "Lens corrections" panel +
    `presetScope` "lens" group. Tests: `param_effects::{lens_distortion_remaps_nonflat_image,
    ca_separates_channels_on_grayscale_edge}` + `params::geom_tests::{lens_active_flag_and_geom_packing,
    lens_remap_center_fixed_and_radially_monotone}`. `LENS_K1/K2/CA_SCALE` in `params.rs` = tuning knobs.
  - **Track A pre-release/signing scaffolding:** empty-library **onboarding CTA** (`LibraryView.tsx`,
    filtered-empty vs truly-empty), **scoped CSP** (`tauri.conf.json`, was `null` — NEEDS in-app + built-
    bundle verification), **env-gated macOS notarization + Windows signing** in `release.yml` (+ new
    `src-tauri/Darkroom.entitlements`; inactive until repo secrets set — stays ad-hoc today).
  - **Module 2 — Presence (clarity/texture/dehaze):** DONE, but via the SIMPLE path — reuse the
    existing input mip chain for the multi-scale blur (NO new binding/pipeline/scratch; extended
    `ExtraUniform` @9 `local`, `apply_local_contrast` in develop.wgsl). Next free binding still = 15.
    Separable-Gaussian is the deferred quality upgrade. Tuning knobs `LC_K_*` in develop.wgsl.
  - **NOT done:** CI browser-e2e job — every `e2e/tests/*` uses the real-backend `tauriPage` bridge
    (needs the app + CR3 fixtures + socket), so a headless "mocked browser" CI job would run no
    meaningful tests; gating e2e in CI needs a headless-app launch harness (larger than planned).
  - **PENDING:** in-app visual QA (lens/CA + Presence look on real CR3; tune `LENS_*`/`LC_*`; CSP must
    render thumbs+canvas in `tauri dev` AND a built dmg) · push `main` to origin.
- Gates green on the branch: `cargo test --workspace`, `cargo clippy --workspace`, `npx tsc`, `npm run build`.

## Repo state sync (2026-06-26) — docs had drifted from `main`

- **`main` = `f7445df`; `origin/main` = `1cbb3e3` (v0.1.1 released); 2 commits unpushed** (`e880fda`
  GPU device-acquisition hardening, `f7445df` progressive full-screen preview). The earlier claim
  "origin/main = f663ee0, only the cleanup is unpushed" is **stale**.
- **Merged to `main` since these docs were last accurate** (and previously undocumented here):
  `feat/windows-packaging` (NSIS + DirectML AI + Windows CI, `55ca6fc`), **dedup redesign** (UI
  `175dd44` + pipeline tightening `ab1bcc6`), **diagnostic logging** (`626e447` + fix `e583817`),
  Intel-macOS/beta **CI** config, **v0.1.1 release** (`1cbb3e3`), GPU hardening, progressive preview,
  Windows thumb-protocol fix (`ec3b20a`). (See memory `darkroom-windows-packaging`,
  `darkroom-dedup-redesign`, `darkroom-logging`.)
- **`feat/presets-history` is 100% uncommitted** on top of `main` (`f7445df`): 18 modified + 14 new
  files. Implementation feature-complete; hardening DONE (below); only in-app QA + commit remain.

## DONE (branch `feat/presets-history`, not merged): Develop Presets + Edit-History + Lightroom import

> Plan: `~/.claude/plans/act-as-senior-software-purrfect-glade.md`. All headless gates green:
> `cargo test --workspace` + `cargo fmt --all --check` + `npx tsc` + `npm run build` clean; new code
> clippy-clean (pre-existing warnings in core-dedup/core-import/core-pipeline are untouched). Tier-1
> mock UI QA passed (panel renders, create-dialog + module checklist + masks caveat + save toast,
> History undo/redo, apply-preset enables Undo, 0 console errors). **Pending: in-app GPU/CR3 QA**
> (`npm run tauri dev`) — hover live-preview snappiness, amount-blend look, LR `.xmp` import report on a
> real preset, snapshots surviving restart.

- [x] **`core-preset` crate (pure CPU, no wgpu)** — the sparse merge engine + format-agnostic import.
      `apply_sparse` (Value-level overlay + amount lerp), `ModuleScope` (group→field, Rust source of
      truth; `presetScope.ts` mirrors it; drift-guarded by a test in `commands.rs`), `PresetIr` +
      `map::ir_to_sparse` (CLEAN/APPROX/DROPPED `ImportReport`), `Registry`/`PresetImporter` trait.
      Importers: `formats/lr_xmp.rs` (roxmltree, `crs:` ns, tone-curve `rdf:Seq`) +
      `formats/lr_template.rs` (minimal Lua-table parser, reuses `build_ir`). Golden tests:
      `tests/{preset,lr_import}.rs` (+ fixtures `sample.xmp`/`sample.lrtemplate`).
- [x] **Sparse-per-field model (review C2 fix):** a preset stores ONLY touched top-level fields; apply
      overlays just those (never resets `toneAmount=100` or existing masks). `replace_all` = base from
      `DevelopParams::default()`. Typed round-trip happens in `src-tauri` (keeps wgpu out of the parser).
- [x] **LR fidelity = best-effort + honest report:** absolute WB `Temperature` + color-grade/split-tone
      DROPPED (no anchor / incompatible `cb_rgb` gain-power channels); basic-tone sliders APPROX; HSL
      1:1 by color index; tone curve `/255`; only relative Tint nudge + un-rotated crop imported.
- [x] **DB:** migration `015_presets` (sparse `params` + `field_keys` + builtin/favorite/group) +
      `016_develop_snapshots` (full-params named snapshots, `ON DELETE CASCADE`). `core-library/
{presets,snapshots}.rs` CRUD; 5 bundled built-ins seeded at setup (`resources/presets/*.json`).
- [x] **IPC:** `presets_{list,get,save,update,delete,duplicate,apply,export,import_file}`,
      `develop_apply_settings` (copy/paste), `snapshot_{list,create,restore,rename,delete}` — all in
      `commands.rs`, registered in `lib.rs`. Apply/restore return merged params (NOT persisted; FE commits).
- [x] **History = hybrid:** in-memory session undo/redo ring in `store/develop.ts` (⌘Z/⌘⇧Z; burst-coalesced
      so a slider drag = one step; cleared on image change) + persistent named snapshots (DB). No
      per-commit history table (keeps `user_events` analytics log separate; no WAL churn).
- [x] **Frontend:** left `DevelopSidePanel` (Presets | History tabs, collapsible) in `DevelopView`;
      `PresetsPanel` (grouped, ★/⋯ menu, amount slider, hover live-preview, copy/paste),
      `CreatePresetDialog` (module checklist + masks geometry caveat), `ImportReportModal`,
      `HistoryPanel` (undo/redo/revert/reset + snapshots), `usePresets`/`useHistory` hooks. CommandPalette
      rows (⌘⇧N/C/V). "Reset all" renamed "Reset to default".

### NEXT — verify · harden · extend (continue in a new session)

**Harden — DONE (2026-06-26, headless-verified, still uncommitted):** all 7 review items landed; gates
re-run green (`cargo test --workspace` 0 failed · `clippy --workspace --examples -D warnings` ·
`fmt --check` · `tsc` · `npm run build`). Also fixed a **pre-existing** `core-dedup` clippy error
(`explicit_counter_loop` at `lib.rs:393`, from merged main commit `ab1bcc6`) that was blocking the
clippy gate — unrelated to presets; commit it separately if desired.

- [x] **Validate imported/saved params at write time** — `validate_sparse_params` (round-trips
      `apply_sparse(default, sparse)` → `DevelopParams`) wired into `presets_save` + `presets_import_file`.
- [x] **Built-in self-check test** — `commands.rs::preset_tests::builtin_presets_are_valid`
      (params ⊆ field_keys ⊆ `all_field_keys` + deserializes).
- [x] **src-tauri merge unit test** — `applying_a_sparse_preset_leaves_untouched_fields_intact` (the mod
      `preset_scope_tests` was renamed `preset_tests`). [Earlier "src-tauri has 0 tests" was already stale —
      4 inline tests existed.]
- [x] **XMP parser robustness** (`lr_xmp.rs`) — scoped the scan to the crs-bearing `rdf:Description`
      (`node_has_crs`/`collect_crs`), now reads child-ELEMENT crs settings too; ~4 MB import cap
      (`core_preset::MAX_IMPORT_BYTES`, enforced in `Registry::import` + `presets_import_file` via metadata).
- [x] **Lua parser robustness** (`lr_template.rs`) — recursion **depth guard** (`MAX_DEPTH=64`),
      `[[…]]` long-bracket strings, and native-JSON import now **requires `schemaVersion`**
      (`PresetEnvelope.schema_version: Option<i64>`). Tests in `tests/robustness.rs`.
- [x] **Restore/apply vs in-flight drag (review H2)** — `applyPreset`/`pasteSettings`/`restoreSnapshot`
      snapshot the `params` reference and drop a stale async result if it changed (covers mid-drag clobber,
      edit-during-await, AND apply-onto-a-different-image); `hoverSaved` cleared on image change.
- [x] **PV-migration hook** — `core_preset::{migrate_sparse,migrate_full}` (no-op seam today) wired into
      `presets_apply` + `snapshot_restore` (snapshot loader now returns `process_version`).

**Verify (blocking — then commit the branch):**

- [ ] **In-app GPU/CR3 QA** (`npm run tauri dev`): apply a preset to a 2nd already-edited image (untouched
      modules stay intact); amount-blend look; hover live-preview snappiness; copy/paste (⌘⇧C/V); ⌘Z/⌘⇧Z;
      snapshot create→restore **survives an app restart**; import a REAL Lightroom `.xmp` AND `.lrtemplate`
      and read the ImportReport (WB-temp + color-grade must show as **dropped**, not silently applied).
      See the consolidated checklist at the bottom of `HAND_OFF.md`.
- [ ] **Commit + decide merge/push** once QA passes (branch `feat/presets-history` is currently uncommitted).

**Extend (functionality):**

- [ ] **Update/overwrite a user preset** from the current edit; **preset search box** + **drag-reorder**
      (the `sort_order` column already exists) + group rename/merge.
- [ ] **Bulk import** a folder of `.xmp`/`.lrtemplate`; **export/import a whole group** as one bundle.
- [ ] **More formats via the registry** (pure mapping, no pipeline change): RawTherapee `.pp3` (INI),
      Capture One `.costyle`. **`.cube`/HaldCLUT** needs a NEW GPU 3D-LUT stage (binding ≥15) FIRST — until
      then it would import as "dropped (no LUT stage)".
- [ ] **Per-camera default / auto-apply-on-import** preset (LR "default develop settings").
- [ ] **LR local-adjustment import** (GradientBased/CircularGradient/PaintBased → `masks`) — opt-in,
      approximate geometry (the image-relative caveat is already surfaced in the create dialog).
- [ ] **Snapshot hover-preview** + an Amount on paste/snapshot restore; optional **color-grade additive
      remap** on import (the currently-dropped path — rebuild as additive offsets per review C4, validate
      the Sat→offset scale numerically before shipping).
- [ ] NOT needed: a `core-types` leaf crate — `core-preset` is already wgpu-free (Value-level merge).

## IN PROGRESS: Windows GPU/performance optimization

> Goal: make Develop rendering and other heavy paths perform correctly on Windows 10+ systems,
> preferring NVIDIA/DX12 when available, without regressing macOS/Apple Silicon behavior.

- [x] Windows GPU selection/diagnostics: DX12 first, prefer NVIDIA discrete adapter, log selected
      adapter/backend/driver, expose `gpu_status` for debug.
- [x] Reuse viewport render targets/readback buffers instead of allocating them per frame.
- [x] Add preview-source first paint for fit views using `develop_linear_preview`, then warm full-res
      cache and re-render when ready. Full-res/export/canonical output remains authoritative.
- [x] Validate with `cargo fmt --all`, `cargo test --workspace`, clippy, `npx tsc --noEmit`, build.

### Hardening pass — branch `feat/windows-hardening` (DONE headless, in-app Windows QA pending)

> Audit (3 explore agents + Windows best-practice research) found the Windows build already largely
> correct (GPU DX12/Vulkan + adapter tier-sort, ort feature-gating, `Mutex<Session>` serializes `Run`
> → satisfies DirectML's single-thread rule, trash gating, `app_data_dir`). Remaining gaps fixed:

- [x] **Frontend platform util** `src/lib/platform.ts` (`isWindows`/`fmtShortcut`) — Command palette +
      TopBar render **Ctrl+…** instead of `⌘` on Windows; TopBar `paddingLeft` 82→12 (no traffic lights).
- [x] **DirectML defensive config** (`core-analyze/src/models.rs`, `#[cfg(windows)]`):
      `with_memory_pattern(false)` + `with_parallel_execution(false)` (ORT_SEQUENTIAL) before EP register.
- [x] **Surface AI accelerator** — `core_analyze::accelerator()` → `AnalysisStatus.accelerator`
      (real + Intel stub) → Settings "AI acceleration" readout; `run_pass` logs it; `core_analyze`
      added to the logging EnvFilter so an ort DirectML→CPU fallback `warn` is captured.
- [x] **WebView2** `downloadBootstrapper` → `embedBootstrapper` (offline-friendly, +~1.8 MB).
- [x] **Dev examples** (5 in `core-analyze/examples`) use `dirs::data_dir()` (dev-dep) not hardcoded
      `~/Library/Application Support` — runnable on a Windows dev box.
- [x] Headless gates green: `fmt --check`, `tsc`, `npm run build`, `clippy --workspace`(+`--examples`),
      `cargo test --workspace`. (Pre-existing `core-pipeline` `#[cfg(test)]` `needless_range_loop` debt
      surfaces only under `clippy --all-targets`; untouched, out of scope.)
- [ ] **In-app Windows QA** (user has a box): (1) check `target/release/` for any `onnxruntime*.dll`
      next to the exe → if present add to `bundle.resources` (memory says static, so expect none);
      (2) NSIS install + launch; (3) GPU develop on NVIDIA discrete (log shows Dx12, max_tex 16384) +
      iGPU fallback; (4) AI scan → Settings reads **DirectML** (not CPU), no fallback `warn` in log;
      (5) thumbnails via `http://thumb.localhost`; (6) recycle-bin delete; (7) Ctrl shortcuts render.
- [ ] Out of scope (chose Recommended, not Comprehensive): code-signing, CSP hardening, CI runtime smoke.

## DONE (MERGED `01a7b84`): Cleanups & tech-debt — branch `chore/cleanups-viewport-histogram`

> Plan: `~/.claude/plans/do-thorough-analysis-of-velvety-hollerith.md`. `npx tsc --noEmit` +
> `cargo test --workspace` (goldens byte-identical) + `clippy --workspace --examples -D warnings` +
> `npm run build` all clean. Committed `0f1dd88`, merged to `main` `01a7b84` (--no-ff); **not pushed**.

- [x] **Shared `useViewport` hook** — `src/lib/useViewport.ts` (+ `src/lib/canvasPaint.ts` `paintFrame`)
      owns the ~200 LOC of canvas-viewport logic that `Stage.tsx` + `Library/Loupe.tsx` duplicated
      (container measure, zoom/pan, single-flight rAF scheduler, wheel/drag/reset). Behavior-preserving:
      Stage injects crop fit-lock via `transformViewState` + keeps its `renderFn`/preview-paint/overlays;
      Loupe keeps its tiered preview/decode render body. Hook does NOT pre-size the canvas (each render
      body sizes+paints atomically → no flash); skip-if-canvas-not-mounted retries.
- [x] **Whole-crop histogram** — new `develop_histogram` IPC (`commands.rs`, registered `lib.rs`) renders
      the full crop `{0,0,1,1}` at 384² + histograms it, so the panel is correct while zoomed. Factored
      `ensure_full_render_cache` helper out of `develop_render`; removed `develop_render`'s viewport-biased
      histogram emit. `develop_histogram` is **skip-if-cold** (reuses warm full-res cache only — never
      decodes) to avoid a duplicate full-res decode on image open. Frontend (`useDevelop`/`ipc.ts`):
      `developHistogram` wrapper triggered debounced on param + before/after change + first warm render
      (`histogramSeededFor`), never on pan/zoom.
- [x] Doc reconciliation (HAND_OFF/CURRENT_STATE/TODO) — docs had lagged `main` by 8 commits.
- [ ] **In-app visual QA** (`npm run tauri dev` or Tier-1 mock): zoom/pan/reset in Develop Stage +
      Library Loupe (no regression from the hook extraction); whole-crop histogram correct while zoomed + live on slider drag.
- [x] **Committed** (`0f1dd88`) + **merged to `main`** (`01a7b84`, `--no-ff`). Decide on **push**:
      `origin/main` is at `f663ee0`, so this cleanup (2 commits) is the only unpushed work.

## DONE (MERGED `f663ee0`): Unified AI pipeline + post-review fixes — branch `feat/unified-ai-pipeline`

> Merges object detection + faces + captions into ONE manual scan for **10k–100k libraries**. Fix plan:
> `~/.claude/plans/act-as-senior-ai-linear-tome.md`; design + decisions: memory
> `darkroom-unified-ai-pipeline`. `cargo test --workspace` + `clippy` + `npx tsc --noEmit` all clean.
> **MERGED to `main`** (`f663ee0`); only in-app GUI QA remains (below). Supersedes the two separate AI
> passes recorded further down ("AI People/Animal detection accuracy overhaul" + the face pass).
>
> Also MERGED (`595685d`, `feat/import-ordering-keyset-paging`, undocumented until now): capture-date
> ordering (file-mtime fallback), keyset (cursor) pagination for time sorts (migration `011`
> `idx_images_imported`; filename/rating keep OFFSET), client-side sorted-merge import dedup, throttled
> live sidebar. ~500-line `useLibrary.ts` refactor. Memory: `darkroom-library-tree-staged-import`.

- [x] **Phase 0** decode-once: `core_raw::preview_with_orientation` (one JPEG decode → native ≤1024 +
      oriented ≤1536); pixel-equivalence test `core-raw/tests/decode_once.rs` (justifies "no model
      re-validation").
- [x] **Phase 1** per-stage dirty-DAG + keyset pagination: `stale_targets`/`stale_count`/
      `present_targets_after` (status='ok' gate, never OFFSET); migration `012` `images(status,id)`.
- [x] **Phase 2** face data-safety: `reconcile_faces` (IoU-match, preserves id/person/confirmed/rejected/
      cover; never drops a person-assigned face); error→retry (no "0 faces" marker on inference error);
      migration `013` invalidates suspect markers via `json_extract`; `faces_delete_all` guarded vs an
      in-flight scan.
- [x] **Phase 3** scalable clustering: `has_dirty_faces` skip + chunked cancel + EXACT pairwise (dropped
      the ~410 MB n×dim matrix); dim-mismatch guard. ANN (instant-distance HNSW) documented for >200 k.
- [x] **Phase 4** coordinator `run_pass` (Phase A detect+faces → `run_clustering` → Phase B deferred
      captions); single `analysis_running` guard + cancel; auto-import trigger REMOVED; `faces.rs` →
      shims; Settings `face_stage_enabled` (default on); Florence built lazily in Phase B.
- [x] **Phase 5** frontend: `faceStageEnabled` IPC + Settings "Detect people" toggle; unified
      `analysis:*` events (`useFaces` rewired off `faces:*`; `faces:models` kept).
- [x] **Review R1–R3** (3 parallel Claude review agents — Codex was usage-limited): fixed
      person-assigned-face deletion, embedding zero-pad, matrix memory, migration brittleness, emit
      spam (÷32), Florence residency, event duplication. +3 regression tests (reconcile / dim-guard /
      json_extract).

### NEXT (post-merge — still open)

- [ ] **In-app GUI QA** (`npm run tauri dev`): one scan runs detection+faces+captions; ONE progress
      bar; People populate before captions; a confirmed/assigned face survives a re-scan; cancel stops
      it; `faces_delete_all` during a scan is refused. (Models ≈ 900 MB object + 190 MB faces on first
      run.) — the only genuinely-blocking item; the branch is already merged.
- [ ] Deferred (optional): full Phase-A/B `run_pass` fn-split (cosmetic — `run_clustering`
      already extracted); ANN clustering (instant-distance) for >~200 k faces; remove the now-dead
      `analyze: bool` param from `commands.rs::index_root_blocking`; optional independent Codex
      cross-check (correctness / perf / clean-code) of the AI pass.

## DONE: ACR tone-curve fit + Color-balance-RGB (develop-fidelity pass) — MERGED `d3e1d3e`

> Branch `feat/acr-curve-colorbalance`, **merged to `main`** (`d3e1d3e`). Plan:
> `~/.claude/plans/act-as-senior-software-moonlit-zephyr.md`. Deep notes: memory
> `darkroom-acr-curve-colorbalance`. All workspace tests + clippy + npm build green.

- [x] **Base tone curve fit to real ACR** (`core-pipeline/src/base_curve_ref.rs` = Adobe universal
      default curve, 1025 pts from RawTherapee `dcp.cc`; verified via `exiftool` the R7 Adobe-Standard
      DCP has no embedded ProfileToneCurve → renders through this universal curve). Maps mid-grey
      0.18→0.388 (≈65% sRGB) so unedited imports match the Lightroom default brightness (~+1.3 EV vs
      before). `acr_curve` blends flat Reinhard (amount=0) → ACR fit (amount=1=default `tone_amount`).
      Codex-reviewed C¹ asymptotic highlight shoulder (x>0.875; the `1−k/(x+k)` form can't pass (1,1)).
      Golden `param_effects::base_curve_tone_response` (0.18→8-bit 167) + `acr_fit_tests` (RMS L\* < 2.0).
      `BASELINE_GAIN` (`params.rs`, default 1.0, rides `ExtraUniform.texel.z`) = the visual-QA brightness
      knob; `examples/measure_midgrey.rs` reports mid-grey placement. `PROCESS_VERSION` 3→4.
- [x] **Color-balance-RGB** (`@binding(14)` `CbRgbUniform`) — faithful subset of darktable
      `colorbalancergb`: 4-way (global offset / shadows lift / highlights gain / midtones per-channel
      power) + scene-linear contrast + global chroma, in the GPT-5.5-verified Filmlight grading RGB
      (`params.rs::grading_matrices`, round-trip 7e-17), with darktable's exact `opacity_masks`. Runs
      scene-linear BEFORE the base tone operator. `CbRgb::is_identity()`→active flag ⇒ defaults skip the
      round trip (byte-identical render). `ColorBalance.tsx` panel + `useDevelop::onColorBalanceChange`.
      Tests: `grading_space_tests`, `color_balance_*` (GPU). Deferred tail: JzAzBz perceptual sat/
      brilliance, per-band sat, hue-shift, vibrance, gamut LUT.
- [x] Quick win: eyedropper disarmed during crop mode (`MaskOverlay.tsx`).

### NEXT (after this pass, prioritized)

- [ ] **In-app visual QA** (`npm run tauri dev`) — THE #1 pending item. Confirm the brighter ACR
      default + Color balance panel + crop/straighten + Temp/Tint/Sharpen/Vignette on varied real CR3.
      Tune `BASELINE_GAIN` if the default look wants nudging. Math verified headless; look is subjective.
- [ ] **Lightroom `.xmp` preset import** (now unblocked) — new `core-preset` crate mapping `crs:` keys
      → `DevelopParams` (~70%: exposure/WB/contrast/tone-curve/HSL/sat/color-grading). Sidecar JSON can
      grow an XMP-`crs:` bridge.
- [ ] **Clarity / texture / dehaze** (local contrast) — needs a multi-scale (Gaussian/bilateral) blur
      beyond the current 3×3. New binding(s) ≥15.
- [ ] **Color-balance perceptual tail:** JzAzBz/dtUCS saturation + brilliance (PQ EOTF), per-band sat,
      hue-shift, vibrance, gamut soft-clip.
- [ ] Smaller wins: grain (noise LUT), channel mixer (3×3 linear), HaldCLUT/.cube (3D texture).
- [ ] **Codex follow-up** (optional): the plan-mode prose summary didn't flush; the numeric review
      stands (`workspace/logs/codex-curve-review.out`, gitignored). Re-run if extending the math.

## DONE: Viewport render — full-res zoom + near-instant edits + mask overlay

> Branch `feat/viewport-render` (merged). Plan: `~/.claude/plans/snoopy-floating-island.md`. Render
> only the visible viewport at display res (RapidRAW pattern); canvas + server view-rect replaces
> `<img>`+CSS scale (kills WKWebView zoom blur/glitch); mask-layer cache + raw-RGBA transport →
> ~260 ms → ~5 ms per masked edit. 41 core-pipeline tests green, goldens byte-identical, build clean,
> Tier-1 mock QA passed.

- [x] `ViewUniform` `@binding(13)` + `ViewParams`; `render_view` (display-sized viewport);
      `render()` = byte-identical identity wrapper.
- [x] Geometry split `crop_to_source` (crop+zoom+straighten compose); 5 `tests/viewport.rs` vectors.
- [x] Mask-layer cache (`PreparedImage.mask_layer_hash`, `mask::mask_geometry_hash`) — skip pre-pass
      on pan/zoom/scalar edits; cache-correctness test.
- [x] Red overlay shader tint on the packed mask layer; `packed_overlay_layer` index resolution.
- [x] `develop_render` → raw RGBA `[outW][outH][rgba]`; output dims capped 8192; preview-tier LRU
      removed (full-res source cached).
- [x] `lib/viewport.ts` math; canvas `Stage` + `Loupe`; overlays via view-rect; single-flight rAF
      coalescing; double-buffer; crop-aspect-correct natural; `renderTick` → live slider edits.
- [x] `bench_render` example + Codex review (architecture + methodology). 2 code-reviewer passes.

### NEXT (this feature, prioritized)

- [ ] **Real-app visual QA** (`npm run tauri dev`): crisp full-res zoom, red overlay color over a
      real mask, edit snappiness on real photos. (Tier-1 mock is synthetic — can't confirm fidelity.)
- [ ] **B0 native-GPU-surface spike** (go/no-go): CAMetalLayer under a transparent webview, zero
      readback. If go → **Workstream B** (render thread owns Device/Queue/Surface, `run_on_main_thread`
      present, `develop:preview-rendered` event, surface lifecycle). Plan: snoopy-floating-island.md.
- [x] Whole-crop histogram pass — DONE (`chore/cleanups-viewport-histogram`): `develop_histogram` IPC.
- [ ] Tiered source: preview-res for fit, full-res on zoom (faster first-open + fixes fit-view
      minification aliasing, Codex #3).
- [x] Extract a shared viewport hook — DONE (`chore/cleanups-viewport-histogram`): `src/lib/useViewport.ts`.
- [ ] Deferred review nits: derived-key float accumulation; eyedropper-while-cropping guard.

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

## DONE: AI People/Animal detection accuracy overhaul (F1 0.905, 50fb0fc)

> WS1–5 complete & production-wired (D-FINE-M People/Vehicles + MegaDetector-v5a Animals + MobileCLIP-S1
> verifier + Florence-2 caption); label-calibrated person gating → F1 0.905 (v3). Remaining tail is
> deferred polish: ort dylib bundling for a built `.app` (HIGH iff distributing), Florence-2 KV-cache
> (O(n²) decode, acceptable for background), in-app e2e re-analyze QA. Original plan +
> per-WS checklist below kept for reference.
>
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
- [x] **Crop / straighten — DONE** (`feat/tone-operator-crop`): GeomUniform `@binding(12)` +
      `crop_to_source`/`sample_bilinear` 4-tap (the helper already exists), interactive `CropOverlay.tsx`,
      aspect presets + Angle slider, export at true dims via `Crop::export_rect`. Visual-QA pending.
- [ ] **Lens distortion / chromatic-aberration** (the only still-UI-only geometric module; greenfield)
      — reuse `sample_bilinear` for a radial UV / per-channel scale, then **visual QA**.
- [x] **`import_start` lock refactor — DONE** (ea0d66a): brief-lock-snapshot → unlocked copy/hash/
      thumbnail → brief-lock insert; `ImportGuard` RAII gates the FS watcher. Import no longer freezes IPC.
- [ ] **Higher-leverage review items:** dedup `dhash_from_jpeg` — normalize orientation before
      hashing (rotation-sensitive); per-mask WB as a CAT (currently per-channel gain delta);
      bilateral/edge-aware NR (currently a plain 3×3 box → softens edges); dedicated loupe preview
      (≥1536px, not upscaled 512 thumb); cache full-res developed buffer for repeat export.
- [ ] **Viewport leftovers:** ~~whole-crop histogram pass~~ DONE; ~~shared `useViewport` hook~~ DONE
      (both on `chore/cleanups-viewport-histogram`); remaining — tiered preview source (preview-res for
      fit, full-res on zoom); B0 native-GPU-surface spike.
- [ ] **Minor:** aspect-correct the linear gradient mask (`mask_prepass.wgsl::linear_cov`, needs FE+BE
      coord consistency); decide brush `flow` (wire buildup off MAX-blend, or remove from schema+UI).
      (DONE already: real Library histogram, `selectedId` inits null, Stage re-key on `selectedId`,
      eyedropper-vs-crop guard — don't re-flag these.)
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
- [x] **Crop / geometry** (aspect + straighten angle) — DONE (`@binding(12)`, `crop_to_source` +
      `sample_bilinear`, `CropOverlay.tsx`, export at true dims). Visual-QA pending.
- [x] **Base tone operator + Color-balance-RGB** — DONE (`@binding(10/11/14)`; see top section).
- [ ] **Lens distortion / chromatic-aberration** (manual k1 / per-channel radial) — still UI-absent
      (greenfield). Reuse the `sample_bilinear`/UV-remap infra on a fresh binding ≥15 + visual QA.

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
  identity. Detail/vignette = `ExtraUniform` on `@binding(9)`. Bindings 0–14 are now all wired
  (10 ToneOp, 11 base_lut, 12 Geom crop/straighten, 13 View viewport/overlay, 14 CbRgb color-balance);
  **next free = 15**. `ExtraUniform.texel.z` carries `BASELINE_GAIN` (ACR-brightness knob, default 1.0).
- Keep ALL rawler calls in `core-raw` (pinned `=0.7.2`, non-SemVer).
- `rusqlite 0.39` / `rusqlite_migration =2.5.0` pinned for rustc 1.91 — don't bump without checking MSRV.
- wgpu is `=29`; its API differs a lot from older majors (see CURRENT_STATE.md).
