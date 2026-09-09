import { defineConfig } from "@playwright/test";

// Three projects:
//   --project=tauri    drives the REAL app via the plugin socket bridge (all platforms).
//   --project=browser  the plugin's own browser mode (its ipcMocks, not `src/dev/tauriMock.ts`).
//   --project=mock     plain Chromium on the dev server, driving the full `src/dev/tauriMock.ts`.
export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  retries: 0,
  workers: 1, // tauri mode uses a single socket connection
  reporter: [["list"]],
  projects: [
    {
      name: "tauri",
      testIgnore: "mock/**",
      use: { mode: "tauri" } as Record<string, unknown>,
    },
    {
      name: "browser",
      testIgnore: "mock/**",
      use: { mode: "browser" } as Record<string, unknown>,
    },
    // Tier-1: plain Chromium against the Vite dev server, so `src/dev/tauriMock.ts` installs
    // itself. It gates on `!("__TAURI_INTERNALS__" in window)`, and the tauri-playwright fixture
    // injects that object in BOTH of its modes — so a spec that needs the 300-handler mock must
    // use Playwright's own `page` fixture, not `tauriPage`.
    {
      name: "mock",
      testMatch: "mock/**/*.spec.ts",
      use: { baseURL: "http://localhost:1420" },
    },
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
