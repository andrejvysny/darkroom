# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Darkroom — local non-destructive RAW photo library + develop editor for macOS. Tauri v2 · Rust
Cargo workspace · React 19 + Tailwind v4 · SQLite (rusqlite) · wgpu/Metal develop pipeline.

> **Read first when resuming:** `CURRENT_STATE.md` (handoff: done/partial/not-done, gotchas),
> `TODO.md` (prioritized next work), `SPEC_V1.md` (full spec). This file is the orientation; those
> are the source of truth for status.

## Commands

```bash
npm install
npm run tauri dev                          # run app; first launch auto-indexes library/2026/
npm run build                              # frontend type-check (tsc) + vite build
npm run tauri build -- --bundles dmg       # ad-hoc-signed .dmg → target/release/bundle/dmg/

cargo test --workspace                     # unit + integration (decode/index/pipeline/import/dedup) over real CR3
cargo test -p core-pipeline --test param_effects   # single integration test (file = test name)
cargo clippy --workspace                   # must stay clean
```

No-GUI validation harnesses (fast feedback without launching the app):

```bash
cargo run -p core-raw      --example decode_gate    # rawler decodes R7 CR3
cargo run -p core-library  --example scan_library   # index all 240 + thumbs (~2s)
cargo run -p core-pipeline --example render_one     # decode → GPU develop → PNG in /tmp
cargo run -p core-pipeline --example export_full    # full-res export → /tmp
```

App data (catalog + thumb cache): `~/Library/Application Support/com.andrejvysny.darkroom/`
(`catalog.db` is WAL — recent rows live in `catalog.db-wal` until checkpoint).

## Architecture

Workspace = `src-tauri` + `crates/*`; frontend at repo root `src/` (deviates from spec's `/ui`
intentionally, reusing the Tauri scaffold). **The frontend never touches the DB or filesystem
directly — all access is typed Rust IPC commands.** The IPC command surface is the contract; it is
enumerated in `CURRENT_STATE.md` ("IPC command surface").

| Crate           | Role                                                                                                                                    |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| `core-db`       | SQLite catalog: DDL (STRICT), migrations, pragmas. **Re-exports `rusqlite`** so every crate links one copy.                             |
| `core-raw`      | rawler decode, embedded thumb/preview, EXIF meta, BLAKE3 hash, capture fingerprint, linear develop. **All rawler calls isolated here.** |
| `core-library`  | indexing (rayon), thumb cache, queries, keywords, collections, culling, edit persistence                                                |
| `core-pipeline` | wgpu/Metal develop pipeline (`develop.wgsl`, prepare/render), PNG/JPEG encode                                                           |
| `core-import`   | copy/move/reference import, date routing (`YYYY/YYYY-MM-DD`), hash-verify, Trash                                                        |
| `core-dedup`    | byte-identical + same-capture grouping, safe resolve → Trash                                                                            |
| `src-tauri`     | IPC commands (`commands.rs`), `thumb://` protocol (`protocol.rs`), managed state (`state.rs`)                                           |

**Frontend:** `App.tsx` → `TopBar` + (`LibraryView` | `DevelopView`) + `CommandPalette` + `Toast`.
Global state in `store/app.ts` + `store/develop.ts` (zustand). IPC wrappers in `lib/ipc.ts`. Data
hooks: `lib/useLibrary.ts`, `views/Develop/useDevelop.ts`, `hooks/useCulling.ts`. Flows:
`lib/{export,importFlow}.ts`.

### Data flow (three paths)

- **Thumbnails:** embedded JPEG → downscale 512px → disk cache keyed by content hash → `thumb://localhost/<hash_hex>?size=N` protocol → `<img>`.
- **Develop preview:** `core-raw::develop_linear` (rawler `RawDevelop` minus SRgb = linear color-managed RGB) cached once per image (`prepare()` uploads to GPU); slider change → `render()` (uniform rewrite + draw + readback) → JPEG bytes → `tauri::ipc::Response` → JS `invoke<ArrayBuffer>` → `URL.createObjectURL`. **Never base64 over IPC.**
- **Export:** re-decode full-res → full-res GPU render → PNG/JPEG → save dialog dest (not cached).

## Hard constraints (do not violate — see CURRENT_STATE.md for detail)

- **Do NOT add padding to the `vec3 wb_gain` uniform** (`params.rs` ↔ `develop.wgsl`). A scalar packs into the vec3 tail per std140/WGSL; it is correct. A past review false-flagged it. Guarded by golden test `param_effects.rs`.
- **All new GPU data must use new bindings**, never alter `ParamsUniform`. Tone curve = `@binding(3)`, HSL `FxUniform` = `@binding(4)`.
- **rawler `=0.7.2`** pinned (non-SemVer; CR3/EOS R7 validated, no LibRaw). Keep every rawler call inside `core-raw`.
- **wgpu `=29`** — API differs substantially from older majors (Instance/device/pipeline-descriptor changes catalogued in CURRENT_STATE.md).
- **rusqlite `0.39` + rusqlite_migration `=2.5.0`** pinned for rustc 1.91 (newer needs ≥1.95). Don't bump without checking MSRV.
- All SQL filters use bound named params (injection-safe) — keep new queries that way.
- CSP is `null` (permissive) in `tauri.conf.json` — required for `thumb://` + inline styles today; harden before any public distribution.

## Status shorthand

V1 complete + validated on 240 real Canon R7 CR3. **UI-only / not wired to GPU pipeline:** Crop/geometry,
Lens corrections, Detail (sharpen/NR) — sliders render but have no effect. Check `TODO.md` before
assuming a develop module is functional.
