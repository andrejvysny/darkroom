import { createTauriTest } from "@srsholmes/tauri-playwright";

// Tier-3 validation harness for the `tauri-v2-ui-testing` skill.
// `tauri` mode drives the REAL app (real Rust backend) over the socket; `browser` mode mocks IPC.
export const { test, expect } = createTauriTest({
  devUrl: "http://localhost:1420",
  mcpSocket: "/tmp/tauri-playwright.sock",
  ipcMocks: {
    // browser-mode only (the tauri project hits the real backend, not these)
    app_library_root: () => "/mock/library",
  },
});
