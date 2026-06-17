# Darkroom — Current State (handoff)

> Snapshot for resuming in a new session. Pairs with `TODO.md` (what's next), `README.md` (overview),
> `SPEC_V1.md` (full spec), and the plan at `~/.claude/plans/act-as-senior-software-piped-meteor.md`.

## TL;DR

V1 is **functionally complete and validated**. All 5 acceptance criteria pass against the real
240 Canon EOS R7 **CR3** files in `library/2026/2026-06-06/`. A signed (ad-hoc) `.dmg` builds.
GPU develop pipeline works at ~2 ms/slider. 7 integration tests (over real CR3) + unit tests pass;
clippy clean; frontend builds clean.

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
- Develop: `develop_get_edit`, `develop_set_edit`, `develop_render` (returns JPEG ArrayBuffer)
- Export: `export_image`
- Culling: `cull_set_rating`, `cull_set_flag`, `cull_set_label`,
  `cull_set_rating_many`, `cull_set_flag_many`, `cull_set_label_many` (batch)
- Keywords: `keywords_list`, `keywords_for_image`, `keyword_add_to_image`,
  `keyword_add_to_images` (batch), `keyword_remove_from_image`, `keyword_delete`
- Collections: `collections_list`, `collections_for_image`, `collection_create`,
  `collection_rename`, `collection_delete`, `collection_add_images`, `collection_remove_images`
- Dedup: `dedup_scan`, `dedup_resolve`
- Import: `import_start`

`QueryParams` filter dimensions: `folder_id`, `min_stars`, `flag`, `color_label`
(`"__none__"` = unlabeled), `keyword_id`, `collection_id`, `import_session_id`, `search`
(filename/camera/lens/keyword), `sort` ∈ {capture_desc|asc, filename|\_desc, rating_desc|asc,
imported_desc|asc}.

- Protocol: `thumb://localhost/<content_hash_hex>?size=N`
- Events: `import:progress {done,total}`, `import:done {ImportStats}`

### Data flow

- **Thumbnails:** `core-raw` extracts embedded preview JPEG → downscale 512px → disk cache keyed by hash → `thumb://` protocol → `<img>`.
- **Develop:** `core-raw::develop_linear` (rawler demosaic + our own camera→**linear wide-gamut ProPhoto** map via `clip_negative`, keeping >1.0 highlight headroom) cached once per image (`prepare()` uploads to an `Rgba32Float` texture); slider change → `render()` (uniform rewrite + draw + readback) → JPEG → `ipc::Response` → `createObjectURL` → stage `<img>`. The shader converts ProPhoto→sRGB only at the display transition.
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
ProPhoto→sRGB at the display transition. Scene highlight headroom preserved (`clip_negative`) + soft
rolloff (no hard pre-OETF clamp). **Kelvin white balance** via Planckian locus (Kim 2002) + Bradford
CAT on `@binding(8)` (GPT-5.5-reviewed; `wb_matrix(0,0)` is exact identity). Independent endpoint
blacks/whites. **Detail** (3×3 unsharp sharpen + luma/color NR) + **Lens vignette** on `@binding(9)`.

**Partial / UI-only (NOT wired — geometric, need bilinear remap + visual QA):** Crop/geometry
(aspect + straighten angle), Lens distortion / chromatic-aberration. Sliders render but have no effect.

**Not done (deferred from spec):** keyword hierarchy UI, "recent import" as a true session filter,
per-display ICC, RCD/AMaZE demosaic, Windows/Linux, notarization, CSP hardening. (Thumbnail LRU
eviction and FS-watcher reconciliation are DONE — see `thumbs.rs::evict_to` and `src-tauri/watch.rs`.)

## Known issues / caveats

- `import_start` holds the `db` lock across the entire multi-file import (copy/move/hash/thumbnail),
  freezing all DB-backed IPC for the duration — annoying-not-dangerous for single-user; refactor
  deferred. (NOTE: `develop_render` does **not** hold the cache lock during decode — it decodes +
  GPU-prepares unlocked, locking only the brief render+readback. An earlier doc claim was stale.)
- Loupe uses the 512px cached thumb upscaled (no dedicated larger preview yet).
- Export re-decodes full-res (≈1.6s) each time; not cached.
- Unsigned dmg blocked by Gatekeeper on other Macs (`xattr -dr com.apple.quarantine`).

## Suggested next steps (priority order)

1. Wire **tone curve** + **HSL** into the WGSL shader (biggest develop-fidelity gap).
2. Wire **before/after** toggle (render with `DEFAULT_PARAMS`) + per-module reset polish.
3. **CSP hardening** + capabilities tightening (pre-distribution).
4. Thumbnail **LRU eviction**; dedicated **loupe preview** (≥1536px) generation.
5. FS **watcher** + move reconciliation by hash (spec §9).
6. Detail (sharpen/NR) + lens corrections + crop in the export pipe.
7. Developer-ID sign + **notarize** for distribution.
