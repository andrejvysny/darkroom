import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

// The legacy app mirrors its localStorage preferences into the native settings bridge:
// `settings_bridge_write` at startup and after every bridged change, `settings_bridge_export`
// for the "Export settings…" file. The native importer requires the exact envelope and the RAW
// stored strings — so the spec pins the whole snapshot, not just the call.

const ALL_SEEDED: Record<string, string> = {
  "darkroom.bootstrapped": "1",
  "darkroom.autoUpdateCheck": "1",
  "darkroom.suggestions.show": "0",
  "darkroom.export.dir": "/Users/test/Pictures/Exports",
  "darkroom.export.presets": JSON.stringify([
    { name: "Square", template: "{filename}_sq" },
  ]),
  "darkroom.export.last": JSON.stringify({
    template: "{filename}",
    format: "jpeg",
    quality: 90,
  }),
};

type Snapshot = {
  format: string;
  version: number;
  capturedAtMs: number;
  values: Record<string, string | null>;
};

type BridgeCall = {
  cmd: string;
  payload?: { snapshot?: Snapshot; path?: string };
  t: number;
};

function snapshotOf(call: BridgeCall): Snapshot {
  if (!call.payload?.snapshot) {
    throw new Error(`${call.cmd} call carried no snapshot`);
  }
  return call.payload.snapshot;
}

async function bridgeCalls(page: Page, cmd: string): Promise<BridgeCall[]> {
  return page.evaluate(
    `(window.__darkroomIpcLog ?? []).filter((c) => c.cmd === ${JSON.stringify(cmd)})`,
  );
}

async function waitForBridgeCall(page: Page, cmd: string): Promise<BridgeCall> {
  await expect
    .poll(
      async () => (await bridgeCalls(page, cmd)).length,
      { message: `${cmd} never reached the mock IPC log` },
    )
    .toBeGreaterThan(0);
  return (await bridgeCalls(page, cmd))[0];
}

async function openSettings(page: Page): Promise<void> {
  await page.click('[aria-label="Settings"]');
  await expect(
    page.getByText("Automatically check for updates on launch"),
  ).toBeVisible();
}

test("startup writes the raw localStorage snapshot to the bridge", async ({ page }) => {
  await page.addInitScript((seed) => {
    for (const [k, v] of Object.entries(seed)) window.localStorage.setItem(k, v);
  }, ALL_SEEDED);
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");

  const call = await waitForBridgeCall(page, "settings_bridge_write");
  expect(call.payload?.snapshot).toEqual({
    format: "darkroom-web-settings",
    version: 1,
    capturedAtMs: expect.any(Number),
    values: { ...ALL_SEEDED },
  });
});

test("keys never written are captured as null", async ({ page }) => {
  const partial = { ...ALL_SEEDED };
  delete partial["darkroom.export.last"];
  delete partial["darkroom.export.presets"];
  await page.addInitScript((seed) => {
    for (const [k, v] of Object.entries(seed)) window.localStorage.setItem(k, v);
  }, partial);
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");

  const values = snapshotOf(
    await waitForBridgeCall(page, "settings_bridge_write"),
  ).values;
  expect(values["darkroom.export.last"]).toBeNull();
  expect(values["darkroom.export.presets"]).toBeNull();
  // The seeded ones are still the raw strings.
  expect(values["darkroom.suggestions.show"]).toBe("0");
  expect(values["darkroom.export.dir"]).toBe("/Users/test/Pictures/Exports");
});

test("toggling a bridged preference triggers a newer write", async ({ page }) => {
  await page.addInitScript((seed) => {
    for (const [k, v] of Object.entries(seed)) window.localStorage.setItem(k, v);
  }, ALL_SEEDED);
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  const firstCaptured = snapshotOf(
    await waitForBridgeCall(page, "settings_bridge_write"),
  ).capturedAtMs;

  await openSettings(page);
  await page.click("[data-testid=settings-auto-update]"); // "1" → "0"

  await expect
    .poll(
      async () => (await bridgeCalls(page, "settings_bridge_write")).length,
      { message: "toggling auto-check did not trigger a second bridge write" },
    )
    .toBeGreaterThan(1);
  const calls = await bridgeCalls(page, "settings_bridge_write");
  const second = snapshotOf(calls[calls.length - 1]);
  expect(
    second.capturedAtMs,
    "the newer snapshot must carry a strictly larger capturedAtMs",
  ).toBeGreaterThan(firstCaptured);
  expect(second.values["darkroom.autoUpdateCheck"]).toBe("0");
});

test("a failed save barrier is announced through the toast", async ({ page }) => {
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  await page.evaluate(
    `window.__darkroomEmit("app:save-barrier", {
       reason: "quit",
       message: "The editor did not acknowledge the save within 1500 ms, so this quit was cancelled.",
     })`,
  );
  await expect(page.getByText("so this quit was cancelled")).toBeVisible();
});

test("Export settings… writes the snapshot to the chosen file", async ({ page }) => {
  await page.addInitScript((seed) => {
    for (const [k, v] of Object.entries(seed)) window.localStorage.setItem(k, v);
  }, ALL_SEEDED);
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  await waitForBridgeCall(page, "settings_bridge_write");

  await openSettings(page);
  await page.click("[data-testid=settings-export]");

  const call = await waitForBridgeCall(page, "settings_bridge_export");
  // The mock save dialog answers with a fixed path; the command must carry it plus the snapshot.
  expect(call.payload?.path).toBe("/tmp/darkroom-settings.json");
  expect(call.payload?.snapshot).toEqual({
    format: "darkroom-web-settings",
    version: 1,
    capturedAtMs: expect.any(Number),
    values: { ...ALL_SEEDED },
  });
});
