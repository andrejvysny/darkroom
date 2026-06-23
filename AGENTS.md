# Repository Guidelines

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

## Commit & Pull Request Guidelines

History primarily follows concise Conventional Commit subjects: `feat(develop): ...`, `fix(library): ...`, or `docs: ...`. Keep commits imperative and scoped. Pull requests should explain user-visible behavior, list validation commands, link relevant issues, and include screenshots or recordings for UI changes. Never commit local catalogs, generated test artifacts, or bulk RAW libraries.
