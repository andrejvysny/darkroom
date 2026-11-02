import { test, expect, type Page, type TestInfo } from "@playwright/test";
import { writeFile } from "node:fs/promises";

const VIEWPORT = { width: 1440, height: 900 };

async function capture(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ path, animations: "disabled" });
  await testInfo.attach(name, { path, contentType: "image/png" });
}

async function openApp(page: Page, testInfo: TestInfo): Promise<void> {
  await page.setViewportSize(VIEWPORT);
  await page.addInitScript(() => {
    localStorage.setItem("darkroom.bootstrapped", "1");
  });
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  // The legacy development mock omits model inventory; capture an explicit empty inventory.
  await page.evaluate(() => {
    const bridge = (window as unknown as {
      __TAURI_INTERNALS__: {
        invoke: (command: string, payload?: unknown, options?: unknown) => Promise<unknown>;
      };
    }).__TAURI_INTERNALS__;
    const invoke = bridge.invoke.bind(bridge);
    bridge.invoke = (command, payload, options) =>
      command === "models_overview" ? Promise.resolve([]) : invoke(command, payload, options);
  });
  await page.evaluate(
    "import('/src/store/models.ts').then(({ useModelsStore }) => useModelsStore.getState().setOverview([]))",
  );
  const display = await page.evaluate(() => ({
    viewport: { width: innerWidth, height: innerHeight },
    devicePixelRatio,
    userAgent: navigator.userAgent,
    modelInventory: "empty inventory seeded by capture fixture",
  }));
  const environmentPath = testInfo.outputPath("capture-environment.json");
  await writeFile(environmentPath, JSON.stringify(display, null, 2));
  await testInfo.attach("capture-environment.json", {
    path: environmentPath,
    contentType: "application/json",
  });
}

async function captureViews(page: Page, testInfo: TestInfo): Promise<void> {
  await capture(page, testInfo, "library");

  const cells = page.locator("[data-testid=thumb-cell]");
  await cells.nth(0).click();
  await expect(page.locator("aside").filter({ hasText: "Camera" })).toBeVisible();
  await capture(page, testInfo, "library-selected-metadata");

  await page.getByTestId("nav-develop").click();
  await expect(page.getByTestId("slider-exposure")).toBeVisible();
  await capture(page, testInfo, "develop");

  await page.getByTestId("nav-dedup").click();
  await expect(page.getByText("Duplicates", { exact: true }).first()).toBeVisible();
  await capture(page, testInfo, "dedup");
}

async function captureDialogs(page: Page, testInfo: TestInfo): Promise<void> {
  await page.getByTestId("nav-library").click();
  await page.getByRole("button", { name: "Import", exact: true }).click();
  await expect(page.getByText("Select source folder")).toBeVisible();
  await capture(page, testInfo, "import-dialog");
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: "Export", exact: true }).click();
  await expect(page.getByText("Destination folder")).toBeVisible();
  await capture(page, testInfo, "export-dialog");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();

  await page.getByTestId("run-scan").click();
  await expect(page.getByTestId("scan-modal")).toBeVisible();
  await capture(page, testInfo, "ai-scan");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();

  await page.getByRole("button", { name: "Settings" }).click();
  await expect(page.getByText("Library location")).toBeVisible();
  await capture(page, testInfo, "settings");
  await page.getByText("AI & Models", { exact: true }).scrollIntoViewIfNeeded();
  await capture(page, testInfo, "settings-ai-models");
  await page.getByRole("button", { name: "Manage models…" }).click();
  await expect(page.getByText("AI Models", { exact: true })).toBeVisible();
  await capture(page, testInfo, "model-manager-empty-inventory");
  await page.locator('button[title="Close"]').click();
}

async function captureMergeSurfaces(page: Page, testInfo: TestInfo): Promise<void> {
  const cells = page.locator("[data-testid=thumb-cell]");
  // Use normal selection UI; opening the panorama modal stages work but does not merge files.
  await cells.nth(0).click();
  await cells.nth(1).click({ modifiers: ["ControlOrMeta"] });
  await expect(page.getByText("2 selected", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Merge to HDR" })).toBeVisible();
  await capture(page, testInfo, "selection-hdr-panorama-actions");
  await page.getByRole("button", { name: /Merge to Panorama/ }).click();
  await expect(page.getByText("Merge to Panorama", { exact: true })).toBeVisible();
  await capture(page, testInfo, "panorama-modal");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
}

test("capture representative mock UI migration baseline", async ({ page }, testInfo) => {
  await openApp(page, testInfo);
  const cells = page.locator("[data-testid=thumb-cell]");
  const imageIds = await cells.evaluateAll((nodes) =>
    nodes
      .slice(0, 2)
      .map((node) => Number(node.getAttribute("data-image-id"))),
  );
  expect(imageIds).toHaveLength(2);
  await captureViews(page, testInfo);
  await captureDialogs(page, testInfo);
  await captureMergeSurfaces(page, testInfo);
});

test("capture unreadable saved-edit failure surface", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    (window as unknown as { __darkroomEditUnreadable: boolean })
      .__darkroomEditUnreadable = true;
  });
  await openApp(page, testInfo);
  await page.locator("[data-testid=thumb-cell]").first().click();
  await page.getByTestId("nav-develop").click();
  const banner = page.getByTestId("develop-save-error");
  await expect(banner).toContainText("couldn't read this photo's saved adjustments");
  await capture(page, testInfo, "develop-unreadable-edit");
});
