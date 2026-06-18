# Building the mock backend (any Tauri v2 app)

The mock lets the frontend run in a plain browser by intercepting every Tauri IPC call. It uses
`@tauri-apps/api/mocks` — the official Tauri v2 test mocks (already a transitive dep of any app using
`@tauri-apps/api`; `shouldMockEvents` needs `@tauri-apps/api` ≥ 2.7.0).

## The three pieces

1. **A mock module** (e.g. `src/dev/tauriMock.ts`) — start from `assets/tauriMock.template.ts`.
2. **Entry-point wiring** (`main.tsx`/`main.ts`/`index.tsx`) — install it before the app renders.
3. **(Maybe) a custom-protocol hook** — only if the app loads resources over a custom scheme.

All gated on dev + "no real Tauri runtime", so production builds and the real `tauri dev` shell are
untouched.

## Step 1 — discover what to mock

Run `scripts/tauri-discover-ipc.sh` (or grep yourself). You need four things:

- **`invoke` command names** — `grep -rEh "invoke(<[^>]*>)?\(\s*['\"\`][^'\"\`]+" <frontend>`. Each is
  a handler key. Note the args object each call passes (the mock receives it as the payload).
- **Event names** — `listen`/`once`/`emit` string args (e.g. `"download://progress"`). Needed so
  `listen()` resolves (and optionally so you can emit to drive progress UI).
- **`@tauri-apps/*` imports** — which plugins/APIs are in use (`dialog`, `fs`, `window`, `opener`,
  `os`, `path`, `notification`, …). Each plugin call routes through `invoke` as
  `plugin:<name>|<cmd>` — handle or no-op them.
- **Custom protocols** — any `scheme://…` used as a resource URL (in `<img src>`, CSS `url()`,
  `fetch`, `convertFileSrc`). The browser can't load these; you must rewrite them (Step 3).

Match each handler's return shape to the frontend's expected type (read the IPC wrapper/types file)
so the UI parses it.

## Step 2 — write `installTauriMock()`

`@tauri-apps/api/mocks` exposes:

```ts
mockIPC(cb: (cmd: string, payload?: InvokeArgs) => unknown, opts?: { shouldMockEvents?: boolean }): void
mockWindows(current: string, ...others: string[]): void   // makes getCurrentWindow()/getAll() work
mockConvertFileSrc(osName: "macos" | "linux" | "windows"): void
clearMocks(): void
```

Pattern (the template implements this):

```ts
import {
  mockIPC,
  mockWindows,
  mockConvertFileSrc,
} from "@tauri-apps/api/mocks";
import type { InvokeArgs } from "@tauri-apps/api/core";

const HANDLERS: Record<string, (p: Record<string, unknown>) => unknown> = {
  get_items: () => FIXTURE_ITEMS, // return shape must match the frontend's type
  get_item: (p) => FIXTURE_ITEMS.find((i) => i.id === p.id) ?? null,
  save_item: () => undefined, // void command
  render_thumb: () => makePngArrayBuffer(), // binary → ArrayBuffer
  // ...one entry per discovered command
};

function handle(cmd: string, payload?: InvokeArgs): unknown {
  const h = HANDLERS[cmd];
  if (h) return h((payload ?? {}) as Record<string, unknown>);
  if (cmd.startsWith("plugin:")) return undefined; // dialogs/window/fs/opener: safe no-op
  console.warn(`[tauriMock] unhandled: ${cmd}`);
  return null;
}

export function installTauriMock(): void {
  if ("__TAURI_INTERNALS__" in window) return; // real Tauri runtime present → do nothing
  mockWindows("main"); // use the app's real window label if not "main"
  mockConvertFileSrc("macos");
  // (Step 3) install any custom-protocol hook here
  mockIPC((cmd, payload) => handle(cmd, payload), { shouldMockEvents: true });
  console.info(
    "[tauriMock] active — mock Tauri backend installed (browser test mode).",
  );
}
```

Notes that matter:

- The callback **may be async / return a Promise** — fine for fixtures that need to build an image.
- **Binary results** (commands that return bytes via `tauri::ipc::Response`): return an
  `ArrayBuffer`; the frontend wraps it in a `Blob`/object URL. Generate one with a `<canvas>`
  (`canvas.toDataURL` → bytes) so it's a real, viewable image.
- **Dialogs** (`plugin:dialog|open/save`): default to returning `undefined`/`null` ("cancelled") so
  flows that wait on the dialog result don't hang. Return a fake path if you want to exercise the
  post-dialog flow.
- **Windows**: `mockWindows("main")` injects window metadata so `getCurrentWindow()` works; methods
  like `toggleMaximize()`/`startDragging()` route through `plugin:window|*` → handled by the no-op.
- **Events**: with `shouldMockEvents: true`, `listen()`/`emit()` are mocked (the lib consumes
  `plugin:event|*` itself). Handlers won't fire unless you `emit` — usually fine to leave progress
  bars idle; emit only when testing progress UI (see `references/driving-with-playwright-cli.md`).

## Step 3 — custom protocols (only if used)

Custom schemes (`asset://`, `stream://`, an app-specific `thumb://`, …) have no handler in a browser,
so resource loads fail (`net::ERR_UNKNOWN_URL_SCHEME`). Two ways to fix, best first:

1. **Hook the URL builder (preferred, covers `<img>` AND CSS `background:url()`).** Most apps build
   these URLs in one helper (often where `convertFileSrc` or a `xxxUrl()` function lives). Add a
   dev-only branch that returns a placeholder data URL:

   ```ts
   // in the URL-builder, after constructing the real url:
   if (import.meta.env.DEV) {
     const mock = (
       window as Window & { __tauriUrlMock?: (u: string) => string }
     ).__tauriUrlMock;
     if (mock) return mock(url);
   }
   return url;
   ```

   and in `installTauriMock()`: `window.__tauriUrlMock = (u) => placeholderDataUrl(u);`
   Generate a placeholder as an **SVG data URL** — but in CSS `url(...)` (unquoted) you must escape
   `(` and `)` (e.g. inside `hsl(...)`), because `encodeURIComponent` leaves them intact and the `)`
   closes the `url()` early. Escape them to `%28`/`%29`.

2. **Override `HTMLImageElement.prototype.src` setter + `Element.prototype.setAttribute`** (when you
   can't touch the URL builder). This catches `<img>` only — CSS `background-image: url(scheme://…)`
   is NOT covered (it's set via inline style, not the img element), so prefer option 1 whenever the
   builder is reachable.

(Playwright's `route()` does **not** intercept custom non-http schemes — they fail before a network
request, so route-based mocking isn't an option here.)

## Step 4 — wire it into the entry point

The mock must install **before any module calls `invoke`**. IPC calls almost always happen in
effects/handlers after mount, so installing at the top of the entry, before render, is enough:

```tsx
async function bootstrap() {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { installTauriMock } = await import("./dev/tauriMock");
    installTauriMock();
  }
  // ...framework render (createRoot(...).render(<App/>), createApp().mount(), etc.)
}
void bootstrap();
```

- `import.meta.env.DEV` ⇒ Vite tree-shakes the whole block (and the dynamic import) from production.
- The runtime `!("__TAURI_INTERNALS__" in window)` ⇒ inert inside real `tauri dev` (Tauri injects
  internals before your bundle runs).
- Non-Vite bundlers: use the equivalent dev flag (`process.env.NODE_ENV !== "production"`).

Then typecheck (`npx tsc --noEmit` or the project's check) and you're ready to serve.

## Gotchas

- **Find the real window label.** If `getCurrentWindow().label` is read, pass the app's actual label
  to `mockWindows(...)` (often `"main"`; check `tauri.conf.json > app.windows[].label`).
- **Strict TS / lint** (`noUnusedParameters`, `noFallthroughCasesInSwitch`, no `any`): prefer an
  object `HANDLERS` map over a `switch`; type the payload as `Record<string, unknown>` and read off
  it; let unused handlers omit the param (`() => …`).
- **Don't mutate the real contract.** Keep the mock's returns matching the frontend's types; if the
  Rust command changes shape, update the mock to match (the frontend's type file is the source).
- **State across refreshes:** keep fixtures in module scope and mutate them in write-command handlers
  so reads reflect prior writes within a session (e.g. setting a value then filtering by it).

## Rationale & alternatives (why this approach)

- **macOS WKWebView has no WebDriver and no CDP.** `tauri-driver` supports only Linux/Windows.
  `safaridriver` automates Safari, not embedded WKWebViews.
- **Playwright's `webkit` is a browser build, not WKWebView** — green tests there don't prove the
  macOS app renders correctly.
- **Community real-app bridges** exist (`tauri-plugin-playwright` socket mode drives the real webview
  via `webview.eval()` and supports macOS; community WKWebView WebDriver shims) but add an app
  dependency and macOS hardware in CI. Use them only when you specifically need the real engine;
  otherwise the mock is faster, deterministic, and cross-platform.
- For real-backend coverage, keep the Rust unit/integration tests as the source of truth for command
  behavior; the frontend mock only needs to match their signatures.
