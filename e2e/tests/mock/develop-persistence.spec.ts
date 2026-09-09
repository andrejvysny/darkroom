import { test, expect } from "@playwright/test";

// Regression: switching photos mid-edit must not drop the previous photo's save.
//
// `useDevelop.ts` debounces persistence through ONE shared timer keyed on nothing, so
// `debouncedPersist(B, …)` clears the timer that was still holding A's `develop_set_edit`.
// Editing A, switching to B and editing B inside the debounce window therefore loses A's edit
// entirely — nothing else (not even the WAL checkpoint on quit) can recover it, because it never
// left the JavaScript timeout.
//
// Runs against `src/dev/tauriMock.ts` (project `mock`), which records every invocation on
// `window.__darkroomIpcLog`.

const DEBOUNCE_MS = 500;

/** `develop_set_edit` image ids seen so far, in call order. */
async function savedIds(page: import("@playwright/test").Page): Promise<number[]> {
  return page.evaluate(
    `(window.__darkroomIpcLog ?? [])
       .filter((c) => c.cmd === "develop_set_edit")
       .map((c) => c.payload?.imageId)`,
  );
}

test("editing A then switching to B inside the debounce still persists A", async ({
  page,
}) => {
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");

  const ids = await page.evaluate(
    `Array.from(document.querySelectorAll("[data-image-id]"))
       .map((e) => Number(e.getAttribute("data-image-id")))
       .slice(0, 2)`,
  );
  expect(ids.length).toBe(2);
  const [a, b] = ids as number[];

  await page.click(`[data-image-id="${a}"]`);
  await page.click("[data-testid=nav-develop]");
  const exposure = page.locator("[data-testid=slider-exposure]");
  await exposure.waitFor();

  // Ignore everything the image-open path issued; only the edits below matter.
  await page.evaluate("window.__darkroomIpcLog.length = 0");

  const t0 = Date.now();
  await exposure.click({ position: { x: 140, y: 7 } }); // edit A
  await page.click(`[data-image-id="${b}"]`); // switch before A's save fires
  await exposure.click({ position: { x: 40, y: 7 } }); // edit B
  const elapsed = Date.now() - t0;

  // A vacuous pass would be worse than a failure: if the three actions took longer than the
  // debounce, A's timer fired on its own and the race under test never happened.
  expect(
    elapsed,
    `drove the edits in ${elapsed} ms — too slow to exercise the ${DEBOUNCE_MS} ms debounce`,
  ).toBeLessThan(DEBOUNCE_MS - 50);

  await page.waitForTimeout(DEBOUNCE_MS * 4);

  const saved = await savedIds(page);
  expect(saved, `develop_set_edit image ids: ${JSON.stringify(saved)}`).toContain(a);
  expect(saved).toContain(b);
});
