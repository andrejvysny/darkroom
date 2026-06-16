# Darkroom

A local, fast, **non-destructive RAW photo library + develop editor** for macOS. Built per `SPEC_V1.md`.

**Stack:** Tauri v2 · Rust (Cargo workspace) · React 19 + Tailwind v4 · SQLite (`rusqlite`) · **wgpu/Metal** GPU develop pipeline.

> Validated against 240 Canon EOS R7 **CR3** files in `library/2026/`.

## What works (v1)

- **Library** — index watched folders, BLAKE3 identity, EXIF metadata, embedded-JPEG thumbnails
  (240 files indexed in ~2s), virtualized grid, metadata panel, folder nav, loupe zoom/pan.
- **Organize** — filter by rating threshold / flag / color label / keyword / collection / folder,
  8 sort orders, full-text-ish search (filename/camera/lens/keyword); **keywords/tags** (per-image
  editor + batch), **static & smart collections** (saved-predicate), multi-select with a batch
  toolbar.
- **Develop** — real wgpu/Metal pipeline: rawler decode + demosaic + color management → linear buffer,
  GPU adjustments (WB, exposure, contrast, highlights/shadows, saturation, blacks/whites) at **~2 ms/slider**;
  non-destructive edits persisted in the catalog.
- **Culling** — star ratings, pick/reject flags, color labels, keyboard culling loop (applies to the
  whole selection when multiple are selected).
- **Export** — full-resolution PNG / JPEG through the develop pipeline (⌘E / command palette), plus
  **batch export** of a selection to a folder.
- **Import** — copy / move (verified) / reference (mode picker), date-routed `YYYY/YYYY-MM-DD`, hash-verified, dedup-skipping.
- **Dedup** — byte-identical + same-capture grouping, safe resolve to Trash.
- **⌘K command palette**, keyboard shortcuts, packaged signed `.dmg`.

## Architecture

Cargo workspace (`crates/*`) + Tauri app (`src-tauri`) + React frontend (`src/`):

| Crate           | Responsibility                                                                                             |
| --------------- | ---------------------------------------------------------------------------------------------------------- |
| `core-db`       | SQLite catalog (full DDL, STRICT, migrations, pragmas)                                                     |
| `core-raw`      | rawler decode, embedded preview/thumbnail, EXIF metadata, BLAKE3 hash, capture fingerprint, linear develop |
| `core-library`  | watched-root indexing (rayon), thumbnail cache, catalog queries, culling, edit persistence                 |
| `core-pipeline` | wgpu/Metal develop pipeline (WGSL shader, prepare/render), PNG/JPEG encode                                 |
| `core-import`   | copy/move/reference import, date routing, verify, Trash                                                    |
| `core-dedup`    | duplicate grouping + safe resolve                                                                          |
| `src-tauri`     | IPC commands, `thumb://` protocol, managed state                                                           |

The frontend never touches the DB or filesystem directly — all access is via typed Rust IPC commands.
Thumbnails stream over a custom `thumb://` protocol; the develop preview is delivered as JPEG bytes
(`ipc::Response` → `ArrayBuffer` → object URL).

## Develop / run

```bash
npm install
npm run tauri dev      # launches the app; first run auto-indexes library/2026/
```

## Build the .dmg

```bash
npm run tauri build -- --bundles dmg
# → target/release/bundle/dmg/Darkroom_0.1.0_aarch64.dmg
```

The build is **ad-hoc signed** (`signingIdentity: "-"`) and **not notarized** (no Apple Developer cert).
It launches on the build machine; on other Macs, after copying to `/Applications`:

```bash
xattr -dr com.apple.quarantine /Applications/Darkroom.app
```

## Test

```bash
cargo test --workspace      # unit + integration tests (decode/index/pipeline/import/dedup) over real CR3s
cargo clippy --workspace
npm run build               # frontend type-check + build
```

## Notes / v1 scope

- Decode is `rawler =0.7.2` (pinned; CR3/EOS R7 validated). No LibRaw. Other Bayer bodies (Sony/Nikon) are
  latent via rawler but unvalidated here.
- Develop demosaic + color matrix run once on CPU (rawler) into a cached linear buffer; the interactive
  pipeline (and export) run on the GPU.
- Tone curve, HSL hue/luma, detail (sharpen/NR), lens corrections, crop are present in the UI but not yet
  wired to the pipeline (visual-only) — planned next.
- CSP is currently permissive (`null`); harden before any public distribution.
