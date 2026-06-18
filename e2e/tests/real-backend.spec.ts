import { writeFileSync } from "node:fs";
import { test, expect } from "../fixtures";

// Tier-3 validation: runs against the REAL Darkroom app (real Rust backend) via the socket bridge.
//   npm run tauri -- dev --features e2e-testing      (terminal 1)
//   cd e2e && npx playwright test --project=tauri --workers=1   (terminal 2)

test("real app UI booted inside the real webview", async ({ tauriPage }) => {
  await expect(tauriPage.locator("body")).toContainText("Library");
  await expect(tauriPage.locator("body")).toContainText("Develop");
});

test("real Rust backend responds + mock is inactive", async ({ tauriPage }) => {
  // evaluate() takes an EXPRESSION (not statements). One async IIFE returns everything we assert on;
  // invoke() here reaches the REAL backend (real catalog DB + thumb cache).
  const r = await tauriPage.evaluate<{
    hasInternals: boolean;
    hasGlobalTauri: boolean;
    mockHook: string;
    cap: number;
    count: number;
  }>(
    "(async () => ({" +
      " hasInternals: ('__TAURI_INTERNALS__' in window)," +
      " hasGlobalTauri: ('__TAURI__' in window)," +
      " mockHook: typeof window.__darkroomThumbMock," +
      " cap: await window.__TAURI_INTERNALS__.invoke('thumb_cache_cap', {})," +
      " count: await window.__TAURI_INTERNALS__.invoke('library_count', { params: {} })" +
      " }))()",
  );
  expect(r.hasInternals).toBe(true); // real Tauri runtime present
  expect(r.hasGlobalTauri).toBe(true); // withGlobalTauri: true is wired
  expect(r.mockHook).toBe("undefined"); // installTauriMock early-returned → NOT mock mode
  expect(typeof r.cap).toBe("number"); // real thumb-cache command answered
  expect(r.cap).toBeGreaterThan(0);
  expect(typeof r.count).toBe("number"); // real catalog DB answered
  expect(r.count).toBeGreaterThanOrEqual(0);
});

test("native screenshot of the real window", async ({ tauriPage }) => {
  const buf = (await tauriPage.screenshot()) as Buffer; // CoreGraphics capture of the WKWebView
  writeFileSync("/tmp/tier3-real.png", buf);
  expect(buf.length).toBeGreaterThan(1000);
});
