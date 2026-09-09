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

test("a continuous gesture still persists while it is happening", async ({
  page,
}) => {
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  await page.click("[data-testid=thumb-cell]");
  await page.click("[data-testid=nav-develop]");
  const exposure = page.locator("[data-testid=slider-exposure]");
  await exposure.waitFor();
  await page.evaluate("window.__darkroomIpcLog.length = 0");

  // Keep editing for well over MAX_WAIT_MS without ever pausing long enough for the idle debounce.
  const start = Date.now();
  for (let i = 0; Date.now() - start < 2400; i++) {
    await exposure.click({ position: { x: 40 + (i % 12) * 8, y: 7 } });
    await page.waitForTimeout(120); // shorter than the idle debounce, so only max-wait can fire
  }
  const elapsed = Date.now() - start;
  const during = await savedIds(page);

  // Without a max-wait, an eight-second drag would sit entirely unsaved. Two full MAX_WAIT_MS
  // windows elapsed, so two saves must have landed mid-gesture.
  expect(elapsed).toBeGreaterThan(2 * 1000);
  expect(
    during.length,
    `saves during a ${elapsed} ms gesture: ${during.length}`,
  ).toBeGreaterThanOrEqual(2);
});

test("leaving Develop persists the pending edit immediately", async ({
  page,
}) => {
  await page.goto("/");
  await page.waitForSelector("[data-testid=thumb-cell]");
  await page.click("[data-testid=thumb-cell]");
  await page.click("[data-testid=nav-develop]");
  const exposure = page.locator("[data-testid=slider-exposure]");
  await exposure.waitFor();
  await page.evaluate("window.__darkroomIpcLog.length = 0");

  await exposure.click({ position: { x: 120, y: 7 } });
  await page.click("[data-testid=nav-library]"); // unmounts Develop mid-debounce
  await page.waitForSelector("[data-testid=library-search]");
  await page.waitForTimeout(200);

  expect((await savedIds(page)).length).toBeGreaterThan(0);
});
