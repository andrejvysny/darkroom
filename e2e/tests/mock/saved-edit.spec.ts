import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

async function open(page: Page) {
  await page.goto("/e2e/fixtures/saved-edit.html");
  await page.waitForFunction("Boolean(window.savedEditHarness)");
}

test("delayed load for the previous image is ignored after image change", async ({ page }) => {
  await open(page);
  await page.evaluate("window.savedEditHarness.mount(1)");
  await page.waitForFunction("window.savedEditHarness.pending(1)");
  await page.evaluate("window.savedEditHarness.mount(2)");
  await page.waitForFunction("window.savedEditHarness.pending(2)");
  await page.evaluate("window.savedEditHarness.resolve(1, 'p1')");
  const mid = await page.evaluate("window.savedEditHarness.state()");
  expect(mid.kind).toBe("loading");
  expect(mid.marker).toBeNull();
  await page.evaluate("window.savedEditHarness.resolve(2, 'p2')");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().kind"))
    .toBe("ready");
  expect(await page.evaluate("window.savedEditHarness.state().marker")).toBe("p2");
});

test("load failure surfaces the error state with its message", async ({ page }) => {
  await open(page);
  await page.evaluate("window.savedEditHarness.mount(1)");
  await page.waitForFunction("window.savedEditHarness.pending(1)");
  await page.evaluate("window.savedEditHarness.reject(1, 'boom')");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().kind"))
    .toBe("error");
  expect(await page.evaluate("window.savedEditHarness.state().message")).toBe("boom");
});

test("retry re-runs the load and reaches ready", async ({ page }) => {
  await open(page);
  await page.evaluate("window.savedEditHarness.mount(1)");
  await page.waitForFunction("window.savedEditHarness.pending(1)");
  await page.evaluate("window.savedEditHarness.reject(1, 'boom')");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().kind"))
    .toBe("error");
  await page.evaluate("window.savedEditHarness.retry()");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().kind"))
    .toBe("loading");
  await page.waitForFunction("window.savedEditHarness.pending(1)");
  await page.evaluate("window.savedEditHarness.resolve(1, 'p1b')");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().kind"))
    .toBe("ready");
  expect(await page.evaluate("window.savedEditHarness.state().marker")).toBe("p1b");
});

test("onReady fires exactly once per image", async ({ page }) => {
  await open(page);
  await page.evaluate("window.savedEditHarness.mount(1)");
  await page.waitForFunction("window.savedEditHarness.pending(1)");
  await page.evaluate("window.savedEditHarness.resolve(1, 'p1')");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().readyCounts[1]"))
    .toBe(1);
  await page.evaluate("window.savedEditHarness.mount(2)");
  await page.waitForFunction("window.savedEditHarness.pending(2)");
  await page.evaluate("window.savedEditHarness.resolve(2, 'p2')");
  await expect
    .poll(() => page.evaluate("window.savedEditHarness.state().readyCounts[2]"))
    .toBe(1);
  const counts = await page.evaluate("window.savedEditHarness.state().readyCounts");
  expect(counts).toEqual({ "1": 1, "2": 1 });
});
