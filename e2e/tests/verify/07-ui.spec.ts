import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

// End-to-end UI wiring: drive the REAL DOM via the bridge and confirm the real backend reacts.
// The grid is virtualized, so a target cell only exists in the DOM when it's on screen — we filter
// the library to the target's filename first so its cell is rendered and clickable.
const ID = 378; // recon: filename 855A6544.CR3, clean state
const FNAME = "855A6544";

// NOTE: section headers use CSS text-transform:uppercase, and innerText returns the *transformed*
// text ("METADATA"). The dt label "Camera" is not transformed, so we anchor on it instead.
const asideText = (page: { evaluate<R>(s: string): Promise<R> }) =>
  page.evaluate<string>(
    "([...document.querySelectorAll('aside')].map(a=>a.innerText).find(t=>t.includes('Camera'))||'')",
  );

test.beforeEach(async ({ tauriPage }) => {
  await tauriPage.waitForSelector("[data-testid=library-search]");
  await tauriPage.fill("[data-testid=library-search]", FNAME); // bring target into the virtual window
  await tauriPage.waitForSelector(`[data-image-id="${ID}"]`);
});

test("search box filters the result set (photo count both ways)", async ({
  tauriPage,
}) => {
  // currently filtered to FNAME → exactly 1
  await tauriPage.waitForFunction(
    "(document.querySelector('[data-testid=photo-count]')?.innerText||'').startsWith('1 ')",
  );
  // clear → many
  await tauriPage.fill("[data-testid=library-search]", "");
  await tauriPage.waitForFunction(
    "!(document.querySelector('[data-testid=photo-count]')?.innerText||'').startsWith('1 ')",
  );
  const many = await tauriPage.textContent("[data-testid=photo-count]");
  expect(parseInt(many || "0", 10)).toBeGreaterThan(1);
});

test("clicking a thumbnail updates the right info panel", async ({
  tauriPage,
}) => {
  await tauriPage.click(`[data-image-id="${ID}"]`);
  await tauriPage.waitForFunction(
    `([...document.querySelectorAll('aside')].some(a=>a.innerText.includes('Camera')&&a.innerText.includes('${FNAME}')))`,
  );
  const txt = await asideText(tauriPage);
  expect(txt).toContain(FNAME);
  expect(txt).toContain("Camera");
});

test("rating a photo via the star control persists to the backend", async ({
  tauriPage,
}) => {
  await tauriPage.click(`[data-image-id="${ID}"]`);
  await tauriPage.waitForSelector("[data-testid=rating-stars]");
  // SVGElement has no .click() — click the 3rd star by coordinate (real native click).
  const star = async () => {
    const b = await tauriPage.boundingBox(
      "[data-testid=rating-stars] svg:nth-child(3)",
    );
    if (!b) throw new Error("no star bbox");
    await tauriPage.mouse.click(b.x + b.width / 2, b.y + b.height / 2);
  };
  await star(); // 3rd star → rating 3
  await expect
    .poll(
      async () =>
        (await call<{ stars: number }>(tauriPage, "image_meta", { id: ID }))
          .stars,
    )
    .toBe(3);
  await star(); // toggle back to 0 (cleanup)
  await expect
    .poll(
      async () =>
        (await call<{ stars: number }>(tauriPage, "image_meta", { id: ID }))
          .stars,
    )
    .toBe(0);
});

test("double-click opens the loupe; Escape returns to grid", async ({
  tauriPage,
}) => {
  await tauriPage.dblclick(`[data-image-id="${ID}"]`);
  await tauriPage.waitForSelector("[data-testid=loupe]");
  expect(await tauriPage.isVisible("[data-testid=loupe]")).toBe(true);
  await tauriPage.keyboard.press("Escape");
  await tauriPage.waitForFunction(
    "document.querySelector('[data-testid=loupe]')===null",
  );
});

test("open in Develop view via toolbar", async ({ tauriPage }) => {
  await tauriPage.click(`[data-image-id="${ID}"]`);
  await tauriPage.click("[data-testid=nav-develop]");
  await tauriPage.waitForFunction(
    "document.body.innerText.includes('Before / After')",
  );
  expect(
    await tauriPage.evaluate<boolean>(
      `document.body.innerText.includes('${FNAME}')`,
    ),
  ).toBe(true);
  await tauriPage.click("[data-testid=nav-library]");
  await tauriPage.waitForSelector("[data-testid=library-search]");
});
