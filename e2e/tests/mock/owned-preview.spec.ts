import { test, expect } from "@playwright/test";
import type { Page } from "@playwright/test";

async function open(page: Page) {
  await page.goto("/e2e/fixtures/owned-preview.html");
  await page.waitForFunction("Boolean(window.previewHarness)");
}

test("slow load for A completing after owner moved to B never paints A", async ({ page }) => {
  await open(page);
  await page.evaluate("window.previewHarness.mount(1, 'A1')");
  await page.waitForFunction("window.previewHarness.ready('A1')");
  await page.evaluate("window.previewHarness.mount(2, 'B1')");
  await page.waitForFunction("window.previewHarness.ready('B1')");
  await page.evaluate("window.previewHarness.resolve('A1')");
  const mid = await page.evaluate("window.previewHarness.state()");
  expect(mid.shown).not.toBe("A1");
  expect(mid.ownerId).not.toBe(1);
  await page.evaluate("window.previewHarness.resolve('B1')");
  await expect
    .poll(() => page.evaluate("window.previewHarness.state().shown"))
    .toBe("B1");
  expect(await page.evaluate("window.previewHarness.state().ownerId")).toBe(2);
});

test("A errors, fallback for A resolves after move to B — URL revoked, A never shown", async ({
  page,
}) => {
  await open(page);
  await page.evaluate("window.previewHarness.mount(1, 'A1')");
  await page.waitForFunction("window.previewHarness.ready('A1')");
  await page.evaluate("window.previewHarness.reject('A1')");
  await page.evaluate("window.previewHarness.mount(2, 'B1')");
  await page.waitForFunction("window.previewHarness.ready('B1')");
  await page.evaluate("window.previewHarness.resolveFallback(1, 'fbA')");
  await expect
    .poll(() => page.evaluate("window.previewHarness.state().revokes"))
    .toContain("fbA");
  expect(await page.evaluate("window.previewHarness.state().shown")).not.toBe("fbA");
  await page.evaluate("window.previewHarness.resolve('B1')");
  await expect
    .poll(() => page.evaluate("window.previewHarness.state().shown"))
    .toBe("B1");
});

test("A -> B -> A: the first A's load resolving last does not overwrite the second A", async ({
  page,
}) => {
  await open(page);
  await page.evaluate("window.previewHarness.mount(1, 'A1')");
  await page.waitForFunction("window.previewHarness.ready('A1')");
  await page.evaluate("window.previewHarness.mount(2, 'B1')");
  await page.waitForFunction("window.previewHarness.ready('B1')");
  await page.evaluate("window.previewHarness.mount(1, 'A2')");
  await page.waitForFunction("window.previewHarness.ready('A2')");
  await page.evaluate("window.previewHarness.resolve('A2')");
  await expect
    .poll(() => page.evaluate("window.previewHarness.state().shown"))
    .toBe("A2");
  // The abandoned A1/B1 loads "complete" last — even firing their onload directly, the
  // generation guard must keep the second A painted.
  await page.evaluate("window.previewHarness.force('A1')");
  await page.evaluate("window.previewHarness.force('B1')");
  await page.evaluate(`{
    const { promise, resolve } = Promise.withResolvers();
    requestAnimationFrame(() => requestAnimationFrame(resolve));
    promise;
  }`);
  expect(await page.evaluate("window.previewHarness.state().shown")).toBe("A2");
  expect(await page.evaluate("window.previewHarness.state().ownerId")).toBe(1);
});
