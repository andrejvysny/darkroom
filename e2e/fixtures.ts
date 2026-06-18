import { createTauriTest } from "@srsholmes/tauri-playwright";

// Tier-3 harness for Darkroom (tauri-v2-ui-testing skill).
// - `tauri` project  → drives the REAL app (real Rust backend) over the plugin's Unix socket.
// - `browser` project → mocked IPC in Chromium (Tier-1 equivalent), uses ipcMocks below.
//
// The real app must be running with the e2e plugin compiled in:
//   npm run tauri -- dev --features e2e-testing
// then:  cd e2e && npx playwright test --project=tauri --workers=1
export const { test, expect } = createTauriTest({
  devUrl: "http://localhost:1420",
  mcpSocket: "/tmp/tauri-playwright.sock",
  // browser-mode only — the `tauri` project hits the real backend, not these.
  ipcMocks: {
    thumb_cache_cap: () => 536870912,
    library_count: () => 0,
    app_library_root: () => "/mock/library",
  },
});
