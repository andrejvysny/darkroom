---
name: tauri-v2-ui-testing
description: >-
  Drive and manually test ANY Tauri v2 desktop app's UI on macOS — take screenshots, click, type,
  fill forms, drag controls, navigate, and verify flows — by running the app's web frontend in
  Chromium with a mocked Tauri backend, controlled via playwright-cli. Use this WHENEVER you need to
  SEE or INTERACT with a running Tauri app: "test the UI", "screenshot the app / a view", "click
  through the app", "verify my frontend change visually", "does this screen / component work",
  "drive the app", "open the Tauri app in a browser", "manual/visual QA of a Tauri app", or when
  setting up UI testing for a Tauri project. Tauri's webview is WKWebView on macOS (and WebKitGTK on
  Linux), which exposes no WebDriver/CDP, so Playwright cannot attach to the real app — this skill
  uses the mocked-frontend approach (Tier 1) for fast, deterministic interaction on any platform,
  plus a macOS-native screencapture/cliclick fallback (Tier 2) for the real GPU-rendered window.
  Prefer this over trying to point Playwright or tauri-driver at the running Tauri build directly on
  macOS. Works for any frontend framework (React, Vue, Svelte, SolidJS, vanilla).
---

# Tauri V2 UI Testing

A Tauri v2 app = a Rust backend + a web frontend in a system webview, bridged by IPC
(`@tauri-apps/api` `invoke`/`listen` + custom protocols). The webview differs per OS — WKWebView on
macOS, WebKitGTK on Linux, WebView2 on Windows — and **on macOS WKWebView exposes no WebDriver and no
CDP**, so plain Playwright and `tauri-driver` cannot attach to the real macOS app directly. We
therefore offer **three tiers**; pick by what you need to verify:

- **Tier 1 — mocked frontend in Chromium (start here; ~80% of UI work).** Run the app's frontend dev
  server in real Chromium and serve every Tauri IPC call from an in-repo **mock backend** so the UI
  is fully functional with no Rust process. Fast, deterministic, cross-platform, headless-CI-ready;
  covers all UI structure, navigation, forms, and state flows. Driven by `playwright-cli`. **Backend
  is faked.**
- **Tier 2 — native screenshot/click of the real app (macOS).** Coarse, coordinate-based: real
  render + pixel-clicks, no element awareness. For a quick look at the real window when you don't
  want a full bridge. `screencapture` + `cliclick`. See `references/native-macos.md`.
- **Tier 3 — full-app end-to-end with the REAL Rust backend (all platforms).** Element-aware
  automation of the running app via a WebDriver/socket bridge: official `tauri-driver` + WebdriverIO
  on **Linux/Windows**, and a community in-app plugin on **macOS** (so macOS is NOT limited to
  frontend-only — you can drive the real backend there too). See `references/full-app-e2e.md`.

**Which tier?** Verifying layout / component logic / a frontend change → **Tier 1**. Verifying that a
real Rust command actually does the right thing (DB write, file I/O, decode, real render) → **Tier
3**. Just need to eyeball the real macOS window quickly → **Tier 2**. Many projects run Tier 1 on
every PR and Tier 3 as a smaller real-backend smoke matrix.

`playwright-cli` is the controller for Tier 1 (a terminal client for Playwright; run
`playwright-cli --help`, and it ships its own usage skill). This skill provides the Tauri-specific
glue: building the mock, driving the webview UI, and standing up the real-app bridge.

> All paths below are relative to the **project root** (the repo containing `src-tauri/`). The skill
> scripts live at `.claude/skills/tauri-v2-ui-testing/scripts/`.

## Tier 1 — workflow

### 0. Orient (read the project's Tauri config)

```bash
node -e 'const c=require("./src-tauri/tauri.conf.json"); console.log(JSON.stringify({
  productName:c.productName, identifier:c.identifier,
  devUrl:c.build&&c.build.devUrl, beforeDevCommand:c.build&&c.build.beforeDevCommand,
  frontendDist:c.build&&c.build.frontendDist}, null, 2))'
```

This gives the dev server URL/port, the command that starts the frontend, the app's bundle id
(`identifier`, used by Tier 2), and where the frontend lives. (If the config has comments it's JSON5;
read it manually instead.)

### 1. Ensure a dev mock exists (build one if not)

The mock is what makes the frontend work in a plain browser. **Check if one is already wired:**

```bash
grep -rl "mockIPC\|__TAURI_INTERNALS__\|installTauriMock" src app ui frontend 2>/dev/null
```

- **If a dev mock already exists** (some projects ship one), skip to step 2.
- **If not, build one** — this is the core of the skill. Discover the IPC surface, then write a mock
  module from the template and wire it into the frontend entry point:

  ```bash
  .claude/skills/tauri-v2-ui-testing/scripts/tauri-discover-ipc.sh   # lists invoke cmds, events, protocols, plugins
  ```

  Copy `assets/tauriMock.template.ts` into the frontend (e.g. `src/dev/tauriMock.ts`), fill in
  handlers for the discovered commands + fixtures, then wire it into the entry (`main.tsx` /
  `main.ts`) so it installs **before** the app renders, gated so production is unaffected:

  ```ts
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { installTauriMock } = await import("./dev/tauriMock");
    installTauriMock();
  }
  ```

  Full instructions — `@tauri-apps/api/mocks` API, custom-protocol handling, events, binary
  returns, gotchas — are in **`references/building-the-mock.md`**. A complete real example is in
  **`references/example-darkroom.md`** (+ `assets/example-darkroom-mock.ts`).

  Verify it compiles: `npx tsc --noEmit` (or the project's typecheck).

### 2. Serve the frontend

```bash
.claude/skills/tauri-v2-ui-testing/scripts/tauri-dev-web.sh   # reads tauri.conf.json; starts the dev server; prints URL
```

Idempotent — if the dev URL is already serving (e.g. an existing `tauri dev`), it reuses it. A
_browser_ visit to the dev URL always gets the mock, because the browser has no Tauri runtime.

### 3. Open Chromium and drive it

Use a **named session** so every call shares one browser:

```bash
playwright-cli -s=app open --browser=chrome <DEV_URL>     # e.g. http://localhost:1420
playwright-cli -s=app snapshot                            # accessibility tree + element refs (cheap)
playwright-cli -s=app click e15                           # act on a ref / CSS / getByRole(...)
playwright-cli -s=app screenshot --filename=/tmp/app.png  # then Read /tmp/app.png to SEE pixels
playwright-cli -s=app close
```

Confirm the mock is live (look for your install log, or that `invoke`-driven content rendered):

```bash
playwright-cli -s=app --raw console | grep -i mock
```

**Snapshot vs. screenshot — for cost and reliability.** Prefer `snapshot` (a text accessibility tree
with refs) to navigate and read state; it's cheap and gives stable refs to click/type. Take a
`screenshot` (and `Read` the PNG) only when you must _see_ pixels — rendered media, layout/spacing.
Screenshots are token-expensive; don't spam them. Full driving guide + gotchas in
**`references/driving-with-playwright-cli.md`**.

### 4. Cleanup

```bash
playwright-cli -s=app close
```

Leave the dev server for the user, or kill it **only if you started it**. Never kill the user's
`tauri dev`.

**Covers:** all UI structure, routing/navigation, forms, selection, conditional rendering, IPC
_contract_ shapes, event-driven UI (with `shouldMockEvents`), and any flow whose logic lives in the
frontend. Runs headless on any platform/CI. **Cannot:** exercise real Rust command behavior, real
file/OS access, native dialogs/menus, or the real render engine — the mock fakes those. For that
fidelity, go to **Tier 3** (real backend) or, for a quick visual look on macOS, **Tier 2**.

## Tier 2 — native screenshot/click of the real app (macOS)

Read `references/native-macos.md`. TL;DR: run the real app (`npm run tauri dev`), then
`scripts/tauri-native-shot.sh [out.png] [bundle-id]` screenshots the real window (targeted by
**bundle id** from `tauri.conf.json` — never by name, which can collide with other apps). Inject
input with `cliclick c:x,y` / `t:text` (needs Accessibility permission). The web content is a single
opaque `AXWebArea`, so you click by pixel coordinates from a screenshot, not by element — coarse, but
zero extra setup.

## Tier 3 — full-app E2E with the REAL backend (all platforms)

This is the answer to "I need to test the actual app, backend included" — and it works on macOS too,
via an in-app bridge. Read **`references/full-app-e2e.md`** for complete, current setup. In short:

- **Linux / Windows:** official `tauri-driver` + WebdriverIO (W3C WebDriver). Mature, documented.
- **macOS:** a community in-app plugin (no official driver exists). Two strong choices:
  - **Playwright-native — `tauri-plugin-playwright`** (`@srsholmes/tauri-playwright`): its `tauri`
    mode drives the real WKWebView over a socket + CoreGraphics screenshots. Best fit if you're
    already using this skill's Playwright flow. (Its `browser` mode is also an alternative Tier-1
    mock via `ipcMocks`.) **Validated end-to-end on a real Tauri 2.11 app** — proven setup +
    gotchas in the reference; copyable JS side in `assets/tier3-playwright-example/`.
  - **WebdriverIO-native — `tauri-plugin-wdio-webdriver` + `@wdio/tauri-service`**: the most mature
    macOS WebDriver path (`embedded` driver auto-detected); one config across all three OSes.

Caveats (detailed in the reference): macOS bridges are **community + pre-1.0** (not in official Tauri
docs), are **debug-build-only**, and pin versions. They DO drive the real Rust backend — the
exception is TestDriver.ai, which only mocks IPC (Tier-1-equivalent, despite using Playwright).

> Note: Playwright's bundled `webkit` is a separate browser build, **not** the system WKWebView, so
> green Playwright-webkit tests don't prove the real macOS app works — which is exactly why Tier 3
> uses an in-app bridge rather than attaching Playwright to a webkit binary.
