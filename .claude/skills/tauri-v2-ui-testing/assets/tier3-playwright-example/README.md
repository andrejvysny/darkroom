# Proven Tier-3 example (tauri-plugin-playwright)

These three files are a **validated** JS-side setup for real-backend E2E via `tauri-plugin-playwright`
(`tauri` mode → drives the real WKWebView; `browser` mode → mocked IPC). Validated end-to-end on a
real Tauri 2.11 app on macOS: 3/3 real-backend tests green (real `invoke()` answered; native
CoreGraphics screenshot worked).

Copy into the target project's `e2e/` and adapt selectors/commands. They pair with the Rust-side
setup in `references/full-app-e2e.md` (optional dep behind an `e2e-testing` feature, cfg-gated
`tauri_plugin_playwright::init()`, `playwright:default` capability, `withGlobalTauri: true`).

- `fixtures.ts` — `createTauriTest({ devUrl, mcpSocket, ipcMocks })`.
- `playwright.config.ts` — `tauri` + `browser` projects; `webServer` reuses an existing dev server.
- `tests/real-backend.spec.ts` — asserts the real UI booted, the real backend answers `invoke()`,
  the mock is inactive, and a native screenshot is captured.

Run (app must be up via `… tauri dev --features e2e-testing`, browser installed via the repo's
`./node_modules/.bin/playwright install chromium chromium-headless-shell`):

```bash
cd e2e && npx playwright test --project=tauri --workers=1
```

Key API notes proven here: `tauriPage.evaluate(script)` takes an **expression** (use an async IIFE,
not `return` statements); `tauriPage.screenshot()` returns a PNG **Buffer**; keep `--workers=1`.
