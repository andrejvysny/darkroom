# Repository Guidelines

## Tracking & Documentation: Plane Only

Task tracking and durable knowledge live in Plane project **DARKROOM**, not in the repo. Never create or update local `TODO.md`, `PLAN.md`, `HANDOFF.md`/`HAND_OFF.md`, `CURRENT_STATE.md`, status/notes/QA-checklist files, or temp trackers.

- **Tasks:** future work, bugs, QA items and follow-ups are Plane work items (children of the `[Area]` parents; labels `qa`, `needs-dev-mac`, `needs-decision`, `idea` plus area). Check Plane before assuming a feature is done or starting new work, and update item state as you go.
- **Knowledge:** specs, architecture, decisions, dead ends and research are Plane Pages under "Darkroom — Documentation Index" (e.g. "Architecture & Hard Constraints", "Implementation Status & History", "Product Spec v1"). Update the existing page; do not create v2/final/dated duplicates.
- **Ask before saving** to Plane: propose what to record unless the user explicitly asks you to.
- **Stays in Git (code-coupled only):** `CLAUDE.md`, `AGENTS.md`, `README.md`, `docs/macos-signing.md`, `tests/corpus/README.md`, `crates/core-analyze/SPIKE.md`, `docs/astra/*`, `.claude/*`, migrations, and IPC/schema definitions in code. Link to them from Plane instead of duplicating.
- Chat sessions, plans, agent memory and generated temp files are not authoritative.

## Project Structure & Module Organization

Darkroom is a macOS Tauri v2 application. The React 19/TypeScript frontend lives in `src/`; views are grouped under `src/views/`, shared UI under `src/components/`, Zustand stores under `src/store/`, and IPC helpers under `src/lib/`. Native commands and application state live in `src-tauri/src/`. Rust domain code is split into focused workspace crates under `crates/core-*`, including database, RAW decoding, library, pipeline, import, deduplication, and analysis. Rust integration tests sit beside each crate in `crates/*/tests/`; Playwright scenarios live in `e2e/tests/`. Treat `DATA/` and most of `library/` as local, large photo data, not source assets.

## Build, Test, and Development Commands

- `npm ci`: install the locked frontend toolchain.
- `npm run tauri dev`: launch the complete desktop app with the Rust backend.
- `npm run dev`: run only the Vite frontend.
- `npm run build`: run TypeScript checks and produce the frontend build.
- `cargo test --workspace`: run Rust unit and integration tests.
- `cargo clippy --workspace --examples -- -D warnings`: enforce CI Rust linting.
- `npm run tauri build -- --bundles dmg`: create the macOS DMG.
- `npm run tauri build -- --bundles nsis`: create the per-user Windows NSIS installer (build on Windows / `windows-latest`; unsigned). CI builds both via `.github/workflows/release.yml` on a `v*` tag.
- `cd e2e && ../node_modules/.bin/playwright test --project=browser`: run mocked-browser E2E tests. Use `--project=tauri` with the E2E-enabled Tauri app for real-backend verification.

## Coding Style & Naming Conventions

Use strict TypeScript; do not introduce `any`. Follow existing two-space indentation and functional React components with hooks. Name components `PascalCase.tsx`, hooks `useCamelCase.ts`, and utilities `camelCase.ts`. Rust uses standard `rustfmt`, `snake_case` modules/functions, and explicit typed errors. Keep database/filesystem access behind typed Tauri IPC; the frontend must not access either directly. Run `npx tsc`, `cargo fmt --all`, and Clippy before submitting.

## Testing Guidelines

Name Rust integration tests by behavior in `tests/*.rs`; use `*.spec.ts` for Playwright. Add focused regression coverage for behavioral fixes. GPU and RAW tests require macOS/Metal and the committed CR3 fixture; CI sets `DARKROOM_REQUIRE_FIXTURES=1`.

Cross-maker RAW decoding is covered by a **downloadable** corpus rather than committed bytes: `tests/corpus/manifest.toml` names 19 CC0 samples from [raw.pixls.us](https://raw.pixls.us/) (7 makers, Canon/Nikon/Sony/Leica/Google/Apple/Pentax) by path and sha256, `scripts/fetch_raw_corpus.sh [--tier 1|2] [--verify-only]` downloads them into `target/raw-corpus/` (git-ignored, ~367 MB tier 1 / ~678 MB both), and `crates/core-raw/tests/corpus.rs` + `crates/core-library/tests/corpus_index.rs` check every file against the machine-recorded goldens in `tests/corpus/expected.toml`. Both tests skip cleanly with no corpus (`corpus_index` falls back to synthetic DNGs from `core_raw::synth`); `DARKROOM_REQUIRE_CORPUS=1` makes a missing corpus a hard failure, `DARKROOM_CORPUS_TIER=2` adds the extended set, `DARKROOM_CORPUS_QUICK=1` skips the expensive multi-develop checks, and `DARKROOM_RAW_CORPUS` moves the download directory. Goldens are never hand-edited — re-record with `cargo run --release -p core-raw --example corpus_probe -- --record > tests/corpus/expected.toml` (that example is also the test's implementation) and read the diff. Known-red assertions live in each entry's `xfail` list with the reason in a comment; an xfail that starts passing fails the test. See `tests/corpus/README.md`. CI runs this as the `raw-corpus` job on main, nightly, on demand, and on PRs labelled `raw-corpus`.

## Commit & Pull Request Guidelines

History primarily follows concise Conventional Commit subjects: `feat(develop): ...`, `fix(library): ...`, or `docs: ...`. Keep commits imperative and scoped. Pull requests should explain user-visible behavior, list validation commands, link relevant issues, and include screenshots or recordings for UI changes. Never commit local catalogs, generated test artifacts, or bulk RAW libraries.
