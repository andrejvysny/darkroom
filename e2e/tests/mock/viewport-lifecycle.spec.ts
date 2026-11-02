import { test, expect } from "@playwright/test";

test("viewport replacement and unmount retire in-flight paints and redraw intent", async ({ page }) => {
  await page.goto("/e2e/fixtures/viewport-lifecycle.html");
  await page.waitForFunction("Boolean(window.viewportHarness)");
  await page.evaluate("window.viewportHarness.mount('A')");
  await expect.poll(() => page.evaluate<number>("window.viewportHarness.state().renders")).toBe(1);
  await page.evaluate("window.viewportHarness.dirty(); window.viewportHarness.mount('B')");
  await expect.poll(() => page.evaluate<number>("window.viewportHarness.state().renders")).toBe(2);
  await page.evaluate("window.viewportHarness.dirty(); window.viewportHarness.unmount(); window.viewportHarness.release()");
  await page.evaluate("window.viewportHarness.settle()");
  expect(await page.evaluate("window.viewportHarness.state()")).toEqual({ renders: 2, published: 0, pending: 0 });
});

test("viewport remount schedules a fresh frame after StrictMode teardown", async ({ page }) => {
  await page.goto("/e2e/fixtures/viewport-lifecycle.html");
  await page.waitForFunction("Boolean(window.viewportHarness)");
  await page.evaluate("window.viewportHarness.mount('A')");
  await expect.poll(() => page.evaluate<number>("window.viewportHarness.state().renders")).toBe(1);
  await page.evaluate("window.viewportHarness.release(); window.viewportHarness.settle()");
  expect(await page.evaluate<number>("window.viewportHarness.state().published")).toBe(1);
  await page.evaluate("window.viewportHarness.unmount(); window.viewportHarness.settle()");
  expect(await page.evaluate<number>("window.viewportHarness.state().pending")).toBe(0);
});
