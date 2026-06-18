import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  retries: 0,
  workers: 1, // tauri mode uses a single socket connection
  reporter: [["list"]],
  projects: [
    // Drives the REAL app (real Rust backend) via the socket bridge.
    { name: "tauri", use: { mode: "tauri" } as Record<string, unknown> },
    // Mocked-IPC in Chromium (Tier-1 equivalent).
    { name: "browser", use: { mode: "browser" } as Record<string, unknown> },
  ],
  // `tauri dev` already serves vite on :1420; reuse it rather than starting a second.
  webServer: {
    command: "npm run dev",
    port: 1420,
    reuseExistingServer: true,
    cwd: "..",
  },
});
