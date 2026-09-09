import { test, expect } from "@playwright/test";

// A stored develop edit that no longer parses is preserved by the backend: `develop_set_edit`
// refuses to overwrite it without an explicit Reset. But `develop_get_edit` returns defaults for it,
// so without a separate probe the photo looks unedited and every slider move would be silently
// rejected. Develop must say so and stop auto-saving instead.

test("an unreadable stored edit is announced and suspends auto-save", async ({
  page,
}) => {
  await page.addInitScript(() => {
    (window as unknown as { __darkroomEditUnreadable: boolean })
      .__darkroomEditUnreadable = true;
  });
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  await page.click("[data-testid=thumb-cell]");
  await page.click("[data-testid=nav-develop]");

  const banner = page.locator("[data-testid=develop-save-error]");
  await expect(banner).toContainText("couldn't read this photo's saved adjustments");

  const exposure = page.locator("[data-testid=slider-exposure]");
  await exposure.waitFor();
  await page.evaluate("window.__darkroomIpcLog.length = 0");
  await exposure.click({ position: { x: 120, y: 7 } });
  await page.waitForTimeout(1500);

  const saves = await page.evaluate(
    `(window.__darkroomIpcLog ?? []).filter((c) => c.cmd === "develop_set_edit").length`,
  );
  expect(saves, "auto-save must stay off until an explicit Reset").toBe(0);
});
