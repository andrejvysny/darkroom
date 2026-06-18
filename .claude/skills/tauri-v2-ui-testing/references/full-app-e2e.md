# Tier 3 — full-app end-to-end (real Rust backend), all platforms

Tier 1 mocks the backend; Tier 2 only screenshots/pixel-clicks the real macOS window. **Tier 3 drives
the REAL app — real Rust commands, real file/OS access, real render — with element-aware
interaction, on every platform.** Use it to verify behavior the mock fakes: actual IPC results,
DB/filesystem effects, real decode/compute, native dialogs.

The catch is the webview engine differs per OS and only some expose an automation protocol, so the
tool depends on the platform:

| Platform    | Webview             | Automation route                                                   |
| ----------- | ------------------- | ------------------------------------------------------------------ |
| **Linux**   | WebKitGTK           | Official `tauri-driver` → `WebKitWebDriver` (W3C WebDriver)        |
| **Windows** | WebView2 (Chromium) | Official `tauri-driver` → `msedgedriver`; or direct CDP            |
| **macOS**   | WKWebView           | **No official driver** → a **community bridge** (an in-app plugin) |

Official Tauri docs still say WebDriver is "Windows and Linux only"
([v2.tauri.app](https://v2.tauri.app/develop/tests/webdriver/)). macOS full-app E2E is entirely
**community-supplied and pre-1.0** as of mid-2026 — usable, not officially blessed. Pin versions and
validate against your Tauri version.

## Contents

- [Decision guide](#decision-guide)
- [Linux / Windows — official tauri-driver + WebdriverIO](#linux--windows)
- [macOS, Playwright-native — tauri-plugin-playwright (recommended here)](#macos-playwright-native)
- [macOS, WebdriverIO-native — tauri-plugin-wdio-webdriver](#macos-webdriverio-native)
- [Other macOS bridges (table)](#other-macos-bridges)
- [CI](#ci)
- [Caveats](#caveats)
- [Sources](#sources)

## Decision guide

- **You already use this skill's Playwright/`playwright-cli` flow and dev on macOS** → use
  **`tauri-plugin-playwright`** (Playwright-native; its `tauri` mode drives the real WKWebView, and
  its `browser` mode is an alternative way to do Tier-1 mocking with `ipcMocks`). Best fit here.
- **You want one WebDriver setup across Linux/Windows CI** → official **`tauri-driver` + WebdriverIO**.
- **You want WebDriver including macOS, maximum maturity** → **`tauri-plugin-wdio-webdriver` +
  `@wdio/tauri-service`** (WebdriverIO org; `embedded` driver auto-detected on macOS).
- **Frontend logic only / no Mac in CI** → stay on **Tier 1** (mock); it's faster and deterministic.

A common, pragmatic split: Tier 1 (mock) for fast UI logic on every PR + Tier 3 on a smaller matrix
for real-backend smoke tests. Keep Rust unit/integration tests as the source of truth for command
internals.

<a id="linux--windows"></a>

## Linux / Windows — official `tauri-driver` + WebdriverIO

`tauri-driver` is a W3C WebDriver proxy (port 4444) that forwards to the platform's native driver. It
does **not** support macOS (no WKWebView WebDriver; tracking issue
[tauri-apps/tauri#7068](https://github.com/tauri-apps/tauri/issues/7068)).

```bash
cargo install tauri-driver --locked
```

Prerequisites:

- **Linux:** `sudo apt-get install -y libwebkit2gtk-4.1-dev webkit2gtk-driver xvfb` (the
  `webkit2gtk-driver` package provides `WebKitWebDriver`; `xvfb` for headless CI).
- **Windows:** `msedgedriver.exe` matching the installed WebView2/Edge version (use
  [`chippers/msedgedriver-tool`](https://github.com/chippers/msedgedriver-tool) to auto-match, or
  `winget install Microsoft.EdgeDriver -v <ver>`).

Build the binary the driver launches, then point WebdriverIO at it:

```bash
npm run tauri build -- --debug --no-bundle     # → src-tauri/target/debug/<app>(.exe)
```

Minimal `wdio.conf.js` (spawn/kill `tauri-driver` around the session; capability names the binary):

```js
import { spawn, spawnSync } from "child_process";
import os from "os";
import path from "path";
let tauriDriver;
export const config = {
  host: "127.0.0.1",
  port: 4444,
  specs: ["./test/specs/**/*.js"],
  maxInstances: 1,
  capabilities: [
    { "tauri:options": { application: "../src-tauri/target/debug/<app>" } },
  ],
  framework: "mocha",
  reporters: ["spec"],
  onPrepare: () =>
    spawnSync(
      "npm",
      ["run", "tauri", "build", "--", "--debug", "--no-bundle"],
      { stdio: "inherit", shell: true },
    ),
  beforeSession: () => {
    tauriDriver = spawn(
      path.resolve(os.homedir(), ".cargo/bin/tauri-driver"),
      [],
      { stdio: [null, process.stdout, process.stderr] },
    );
  },
  afterSession: () => tauriDriver?.kill(),
};
```

```js
// test/specs/example.e2e.js
describe("app", () => {
  it("greets", async () => {
    expect(await (await $("body > h1")).getText()).toMatch(/hello/i);
  });
});
```

Run: `cd e2e-tests && npm ci && npx wdio run wdio.conf.js` (Linux CI: prefix `xvfb-run`). Use
`@wdio/cli` v9. Full walkthrough: the official guide
[v2.tauri.app/develop/tests/webdriver/example/webdriverio](https://v2.tauri.app/develop/tests/webdriver/example/webdriverio/).

<a id="macos-playwright-native"></a>

## macOS, Playwright-native — `tauri-plugin-playwright` (recommended for this skill)

`@srsholmes/tauri-playwright` + crate `tauri-plugin-playwright` (v0.4.0, June 2026; MIT; ~30★,
pre-1.0). Three modes: `browser` (Chromium + `ipcMocks` — like our Tier 1), **`tauri` (drives the
REAL native webview)**, `cdp` (Windows only). The `tauri` mode runs a **Unix socket server inside the
app**, executes test actions via `webview.eval()` in the real WKWebView, returns results over Tauri
IPC, and screenshots via **CoreGraphics** — macOS is the explicit primary target.

Setup — **VALIDATED end-to-end on a real app (Tauri 2.11, macOS): 3/3 real-backend tests green.** A
proven copy of the JS side is in `assets/tier3-playwright-example/` (fixtures + config + spec):

1. **`src-tauri/Cargo.toml`** — gate behind a feature so it never ships in release:
   ```toml
   [features]
   e2e-testing = ["tauri-plugin-playwright"]
   [dependencies]
   tauri-plugin-playwright = { version = "0.4", optional = true }
   ```
2. **`src-tauri/src/lib.rs`** — register only under the feature:
   ```rust
   #[cfg(feature = "e2e-testing")]
   { builder = builder.plugin(tauri_plugin_playwright::init()); }
   ```
3. **`src-tauri/capabilities/default.json`** — add the permission + widen windows, or every command
   silently times out after 30s:
   ```jsonc
   {
     "windows": ["main", "*"],
     "permissions": ["core:default", "playwright:default"],
   }
   ```
4. **`tauri.conf.json`** — `"app": { "withGlobalTauri": true }` is **required** (the bridge uses
   `window.__TAURI_INTERNALS__`). Note: there's one config, so this affects production too.
5. **JS side:**

   ```bash
   pnpm add -D @srsholmes/tauri-playwright @playwright/test && npx playwright install chromium
   ```

   ```ts
   // e2e/fixtures.ts
   import { createTauriTest } from "@srsholmes/tauri-playwright";
   export const { test, expect } = createTauriTest({
     devUrl: "http://localhost:1420",
     ipcMocks: {
       greet: (a) => `Hello, ${(a as { name?: string })?.name ?? "stranger"}!`,
     }, // browser mode
     mcpSocket: "/tmp/tauri-playwright.sock", // tauri mode
   });
   ```

   `playwright.config.ts` declares two projects — `browser` (`use: { mode: "browser" }`) and `tauri`
   (`use: { mode: "tauri" }`) — and a `webServer` running your `dev` command on the dev port.

   ```ts
   // e2e/tests/app.spec.ts — proven copy in assets/tier3-playwright-example/
   import { writeFileSync } from "node:fs";
   import { test, expect } from "../fixtures";

   test("real app + REAL backend (not a mock)", async ({ tauriPage }) => {
     await expect(tauriPage.locator("body")).toContainText("SomeAppText");
     // evaluate() takes an EXPRESSION, not `return` statements (a `return` syntax-errors in the
     // wrapper → 30s hang). Use an async IIFE for multi-step; a Promise expression is awaited by
     // the bridge. invoke() here reaches the REAL Rust backend.
     const r = await tauriPage.evaluate<{ ok: boolean; value: unknown }>(
       "(async () => ({ ok: ('__TAURI_INTERNALS__' in window)," +
         " value: await window.__TAURI_INTERNALS__.invoke('your_command', {}) }))()",
     );
     expect(r.ok).toBe(true);
     const png = (await tauriPage.screenshot()) as Buffer; // native CoreGraphics; returns a Buffer
     writeFileSync("/tmp/real.png", png);
   });
   ```

6. **Install the Playwright browser, then run the `tauri` project on macOS.** Even though the `tauri`
   project drives the real webview, the Playwright runner still launches a browser — install it with
   the SAME Playwright that `@playwright/test` pins (use the repo's binary, NOT a shadowed global
   `npx playwright`, or you get a build-mismatch "Executable doesn't exist …chromium_headless_shell-N"):
   ```bash
   ./node_modules/.bin/playwright install chromium chromium-headless-shell
   ```
   ```bash
   # terminal 1: build + run the real app (compiles the plugin) and serve the frontend
   npm run tauri -- dev --features e2e-testing       # or: cargo tauri dev --features e2e-testing
   # terminal 2: wait for the socket, then run the real-backend tests
   until [ -S /tmp/tauri-playwright.sock ]; do sleep 1; done
   cd e2e && npx playwright test --project=tauri --workers=1
   ```

Gotchas (validated in practice):

- **`evaluate(script)` is an EXPRESSION**, not statements — `return …`/bare `await` hang for 30s. Wrap
  multi-step logic in an async IIFE: `"(async () => ({ … }))()"`.
- **The `tauri` project still needs a Playwright browser installed** (above), matching `@playwright/test`'s
  pinned build — install via `./node_modules/.bin/playwright`, not a global `npx playwright`.
- **`playwright:default` only compiles when the plugin is present.** It's added to a capability, but a
  build WITHOUT `--features e2e-testing` rejects the unknown permission. So either only build with the
  feature, or keep the permission out of the default capability (add it in an e2e-only capability).
  Missing it at runtime ⇒ 30s hangs.
- **`--workers=1`** in tauri mode (single socket connection).
- Use **`tauriPage.screenshot()`** (returns a PNG Buffer via CoreGraphics) — Playwright's own
  screenshot captures a blank internal page, not the WKWebView.
- **debug builds only**; macOS Screen-Recording permission needed locally (implicit on `macos-latest`
  CI); **pin the version** (API moved 0.1→0.4 in 3 months — and `evaluate`/screenshot APIs may shift).

Repo: [github.com/srsholmes/tauri-playwright](https://github.com/srsholmes/tauri-playwright).

<a id="macos-webdriverio-native"></a>

## macOS, WebdriverIO-native — `tauri-plugin-wdio-webdriver`

Most mature macOS WebDriver path (crate v1.1.0, June 2026; from the WebdriverIO org). Embeds a W3C
WebDriver server in the app; `@wdio/tauri-service` auto-detects `driverProvider: 'embedded'` on macOS
(no external `tauri-driver` binary). Choose this if your team is WebdriverIO-centric and wants one
config spanning Linux/Windows (official driver) **and** macOS (embedded).

```bash
cd src-tauri && cargo add tauri-plugin-wdio-webdriver        # register under #[cfg(debug_assertions)]
npm i -D @wdio/cli @wdio/tauri-service && npx wdio config    # pick Tauri; macOS → 'embedded'
```

```rust
#[cfg(debug_assertions)] { builder = builder.plugin(tauri_plugin_wdio_webdriver::init()); }
```

`wdio.conf.ts`: `services: [['tauri', { application: '/path/to/App.app' }]]`. Docs:
[webdriver.io/docs/desktop-testing/tauri/platform-support](https://webdriver.io/docs/desktop-testing/tauri/platform-support/).

<a id="other-macos-bridges"></a>

## Other macOS bridges

| Tool                                                                                          | Protocol / client          | macOS                          | Maturity (mid-2026) | Notes                                           |
| --------------------------------------------------------------------------------------------- | -------------------------- | ------------------------------ | ------------------- | ----------------------------------------------- |
| [`tauri-plugin-playwright`](https://github.com/srsholmes/tauri-playwright)                    | Socket / **Playwright**    | ✅                             | v0.4.0, ~30★        | Playwright-native; recommended here             |
| [`tauri-plugin-wdio-webdriver`](https://github.com/webdriverio/desktop-mobile)                | W3C / **WebdriverIO**      | ✅ embedded                    | v1.1.0, WDIO org    | Most mature WebDriver path                      |
| [`Choochmeque/tauri-plugin-webdriver`](https://github.com/Choochmeque/tauri-plugin-webdriver) | W3C / WebdriverIO·Selenium | ✅ cross-platform              | v0.2.1, ~30★        | Standalone OSS crate (ports 4444+4445)          |
| [`danielraffel/tauri-webdriver`](https://github.com/danielraffel/tauri-webdriver)             | W3C / WebdriverIO·Selenium | ✅ macOS-only                  | v0.1.3, ~25★        | No releases since Feb 2026                      |
| [CrabNebula `@crabnebula/tauri-driver`](https://docs.crabnebula.dev/plugins/tauri-e2e-tests/) | W3C / WebdriverIO          | ✅ (subscription)              | Commercial          | macOS gated behind paid plan + API key          |
| [`tauri-pilot`](https://github.com/mpiton/tauri-pilot)                                        | JSON-RPC / its own CLI     | ⚠️ claimed, limited validation | v0.7.2, ~54★        | AI-agent oriented; not WebDriver/Playwright     |
| [TestDriver.ai](https://docs.testdriver.ai/v6/apps/tauri-apps)                                | Playwright + vision AI     | ✅                             | Commercial SaaS     | **Mocks IPC** — frontend only, NOT real backend |

All embed a debug-only plugin and launch the real binary (so they drive the real backend) — except
TestDriver.ai, which mocks IPC (frontend-only, despite being Playwright-based).

<a id="ci"></a>

## CI

- **Linux/Windows matrix:** install per-platform prereqs (above), `cargo install tauri-driver
--locked`, build `--debug --no-bundle`, run WebdriverIO (`xvfb-run` on Linux). Full official YAML:
  [v2.tauri.app/develop/tests/webdriver/ci](https://v2.tauri.app/develop/tests/webdriver/ci/).
- **macOS real-app:** runs on `macos-latest` runners. For `tauri-plugin-playwright`: build with
  `--features e2e-testing`, start dev server + binary, poll for the socket, then
  `playwright test --project=tauri --workers=1`. CoreGraphics capture works on the runner without an
  explicit TCC grant.
- Cheapest reliable shape: Tier-1 mock tests on every PR (any runner, headless) + Tier-3 smoke tests
  on a 3-OS matrix nightly/pre-release.

<a id="caveats"></a>

## Caveats

- **macOS full-app E2E is community/pre-1.0** — not in the official Tauri docs. Pin versions; expect
  API churn; validate against your Tauri minor.
- **Debug builds only** for every embedded plugin — keep them behind `optional`/feature or
  `#[cfg(debug_assertions)]` so they never ship in release.
- **WebDriver gives no macOS via the official path** — only the community plugins above do.
- **Don't confuse with Tier 1.** TestDriver.ai and `tauri-plugin-playwright`'s `browser` mode mock
  IPC — useful, but that's Tier-1-equivalent, not real-backend coverage.
- **Single-config side effects:** `withGlobalTauri: true` and added capabilities live in the real
  config; review them before release.

<a id="sources"></a>

## Sources

Official: [WebDriver docs](https://v2.tauri.app/develop/tests/webdriver/) ·
[WebdriverIO example](https://v2.tauri.app/develop/tests/webdriver/example/webdriverio/) ·
[CI](https://v2.tauri.app/develop/tests/webdriver/ci/) ·
[macOS tracking #7068](https://github.com/tauri-apps/tauri/issues/7068).
Community: [tauri-playwright](https://github.com/srsholmes/tauri-playwright)
([lib.rs](https://lib.rs/crates/tauri-plugin-playwright)) ·
[webdriverio/desktop-mobile](https://github.com/webdriverio/desktop-mobile) +
[platform support](https://webdriver.io/docs/desktop-testing/tauri/platform-support/) ·
[Choochmeque/tauri-plugin-webdriver](https://github.com/Choochmeque/tauri-plugin-webdriver) ·
[danielraffel/tauri-webdriver](https://github.com/danielraffel/tauri-webdriver) ·
[CrabNebula](https://docs.crabnebula.dev/plugins/tauri-e2e-tests/) ·
[tauri-pilot](https://github.com/mpiton/tauri-pilot) ·
[TestDriver.ai](https://docs.testdriver.ai/v6/apps/tauri-apps).
