import { writeFileSync } from "node:fs";
import { test, expect } from "../fixtures";

// Tier-3 validation: these run against the REAL app (real Rust backend) via the socket bridge.
// Run with: npx playwright test --project=tauri  (app must be up via `tauri dev --features e2e-testing`)

test("real app UI booted inside the real webview", async ({ tauriPage }) => {
  await expect(tauriPage.locator("body")).toContainText("Library");
  await expect(tauriPage.locator("body")).toContainText("Develop");
});

test("real Rust backend responds + mock is inactive (real runtime)", async ({
  tauriPage,
}) => {
  // evaluate() takes an EXPRESSION (not statements). Use an async IIFE for multi-step logic; a
  // Promise expression is awaited by the bridge. One round-trip returns everything we assert on.
  const r = await tauriPage.evaluate<{
    mockHook: string;
    hasInternals: boolean;
    cap: number;
    count: number;
  }>(
    "(async () => ({" +
      " mockHook: typeof window.__darkroomThumbMock," +
      " hasInternals: ('__TAURI_INTERNALS__' in window)," +
      " cap: await window.__TAURI_INTERNALS__.invoke('thumb_cache_cap', {})," +
      " count: await window.__TAURI_INTERNALS__.invoke('library_count', { params: {} })" +
      " }))()",
  );
  expect(r.hasInternals).toBe(true); // real Tauri runtime is present
  expect(r.mockHook).toBe("undefined"); // installTauriMock() early-returned → NOT mock mode
  expect(typeof r.cap).toBe("number"); // real backend command answered
  expect(r.cap).toBeGreaterThan(0);
  expect(typeof r.count).toBe("number"); // real catalog DB answered
});

test("native screenshot of the real window", async ({ tauriPage }) => {
  const buf = (await tauriPage.screenshot()) as Buffer;
  writeFileSync("/tmp/tier3-real.png", buf);
  expect(buf.length).toBeGreaterThan(1000);
});
