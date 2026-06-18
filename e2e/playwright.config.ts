import { defineConfig } from "@playwright/test";

// Tier-3 config. Two projects:
//   --project=tauri    drives the REAL app via the plugin socket bridge (all platforms).
//   --project=browser  runs the frontend in Chromium with mocked IPC (Tier-1 equivalent).
export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  retries: 0,
  workers: 1, // tauri mode uses a single socket connection
  reporter: [["list"]],
  projects: [
    { name: "tauri", use: { mode: "tauri" } as Record<string, unknown> },
    { name: "browser", use: { mode: "browser" } as Record<string, unknown> },
  ],
  // `tauri dev` already serves Vite on :1420; reuse it instead of starting a second server.
  webServer: {
    command: "npm run dev",
    port: 1420,
    reuseExistingServer: true,
    cwd: "..",
    timeout: 120_000,
  },
});
