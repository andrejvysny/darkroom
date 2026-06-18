// TEMPLATE — copy to your frontend (e.g. src/dev/tauriMock.ts) and fill in the marked spots.
// A dev-only mock Tauri backend so the frontend runs (and is Playwright-testable) in a plain
// browser, where there is no Tauri runtime and `invoke` would throw.
//
// Wire it into your entry point (main.tsx / main.ts), BEFORE the app renders:
//
//   if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
//     const { installTauriMock } = await import("./dev/tauriMock");
//     installTauriMock();
//   }
//
// See references/building-the-mock.md for the full guide (discovery, events, custom protocols).

import {
  mockIPC,
  mockWindows,
  mockConvertFileSrc,
} from "@tauri-apps/api/mocks";
import type { InvokeArgs } from "@tauri-apps/api/core";

// ── 1) Fixtures ──────────────────────────────────────────────────────────────
// Replace with data shaped like what your real commands return (read your IPC/types file).
type Item = { id: number; name: string };
const ITEMS: Item[] = Array.from({ length: 12 }, (_, i) => ({
  id: i + 1,
  name: `Item ${i + 1}`,
}));

// ── 2) Helpers ───────────────────────────────────────────────────────────────
const num = (v: unknown): number =>
  typeof v === "number" ? v : Number(v) || 0;

/** Generate real image bytes for commands that return binary (Tauri `Response`). */
function makeImageArrayBuffer(w = 640, h = 480, label = "MOCK"): ArrayBuffer {
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d");
  if (!ctx) return new ArrayBuffer(0);
  const g = ctx.createLinearGradient(0, 0, w, h);
  g.addColorStop(0, "#3b6");
  g.addColorStop(1, "#249");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  ctx.fillStyle = "rgba(255,255,255,.85)";
  ctx.font = "28px sans-serif";
  ctx.fillText(label, 24, 44);
  const dataUrl = c.toDataURL("image/png");
  const bin = atob(dataUrl.slice(dataUrl.indexOf(",") + 1));
  const u = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i);
  return u.buffer;
}

/** Placeholder for a custom-protocol resource URL (see §4). CSS-url-safe (escapes parens). */
function placeholderDataUrl(url: string): string {
  const hue = [...url].reduce((a, c) => (a + c.charCodeAt(0)) % 360, 0);
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="213">` +
    `<rect width="100%" height="100%" fill="hsl(${hue} 50% 45%)"/>` +
    `<text x="50%" y="50%" fill="white" font-family="monospace" font-size="14" ` +
    `text-anchor="middle" dominant-baseline="middle">mock</text></svg>`;
  const enc = encodeURIComponent(svg)
    .replace(/\(/g, "%28")
    .replace(/\)/g, "%29");
  return `data:image/svg+xml,${enc}`;
}

// ── 3) Command handlers ──────────────────────────────────────────────────────
// One entry per `invoke("cmd")` your frontend calls (run scripts/tauri-discover-ipc.sh).
// The payload is the invoke args object. Match return shapes to your frontend's types.
const HANDLERS: Record<string, (p: Record<string, unknown>) => unknown> = {
  // --- examples; replace with your real commands ---
  list_items: () => ITEMS,
  get_item: (p) => ITEMS.find((x) => x.id === num(p.id)) ?? null,
  save_item: () => undefined, // void command
  render_image: () => makeImageArrayBuffer(640, 480, "MOCK"), // binary → ArrayBuffer
  // greet: (p) => `Hello, ${String(p.name ?? "world")}!`,
};

function handle(cmd: string, payload?: InvokeArgs): unknown {
  const h = HANDLERS[cmd];
  if (h) return h((payload ?? {}) as Record<string, unknown>);
  if (cmd.startsWith("plugin:")) return undefined; // dialog/window/fs/opener/event: safe no-op
  console.warn(`[tauriMock] unhandled command: ${cmd}`);
  return null;
}

// ── 4) Custom-protocol hook (DELETE if your app has no custom protocol) ────────
// If the app loads resources over a custom scheme (e.g. asset://, stream://, app-specific),
// the browser can't fetch them. Best fix: have the URL-builder in your code consult this hook:
//
//   if (import.meta.env.DEV) { const m = window.__tauriUrlMock; if (m) return m(url); }
//
declare global {
  interface Window {
    __tauriUrlMock?: (url: string) => string;
  }
}

// ── 5) Entry point ───────────────────────────────────────────────────────────
export function installTauriMock(): void {
  if ("__TAURI_INTERNALS__" in window) return; // real Tauri runtime → do nothing
  mockWindows("main"); // use your real window label if it isn't "main"
  mockConvertFileSrc("macos");
  window.__tauriUrlMock = placeholderDataUrl; // remove if unused (§4)
  mockIPC((cmd, payload) => handle(cmd, payload), { shouldMockEvents: true });
  console.info(
    "[tauriMock] active — mock Tauri backend installed (browser test mode).",
  );
}
