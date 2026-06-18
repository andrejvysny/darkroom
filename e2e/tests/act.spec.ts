import { writeFileSync } from "node:fs";
import { test, expect } from "../fixtures";

// Tier-3 "manual acting" proof: drive the REAL Darkroom app with native bridge actions
// (click / fill / press in the real WKWebView), and confirm the real app reacts.

test("type into the real search box (native fill)", async ({ tauriPage }) => {
  await tauriPage.waitForSelector("[data-testid=library-search]");
  await tauriPage.fill("[data-testid=library-search]", "canon");
  expect(await tauriPage.inputValue("[data-testid=library-search]")).toBe(
    "canon",
  );
  await tauriPage.fill("[data-testid=library-search]", ""); // reset
});

test("switch Library ↔ Develop via native clicks", async ({ tauriPage }) => {
  // Start in Library (search box only exists there).
  await tauriPage.waitForSelector("[data-testid=library-search]");

  // Click the Develop tab → the real app re-renders the Develop top bar.
  await tauriPage.click("[data-testid=nav-develop]");
  await tauriPage.waitForFunction(
    "document.body.innerText.includes('Before / After')",
  );
  expect(await tauriPage.isVisible("[data-testid=library-search]")).toBe(false);

  // Back to Library.
  await tauriPage.click("[data-testid=nav-library]");
  await tauriPage.waitForSelector("[data-testid=library-search]");
  expect(await tauriPage.isVisible("[data-testid=library-search]")).toBe(true);

  const buf = (await tauriPage.screenshot()) as Buffer;
  writeFileSync("/tmp/tier3-act.png", buf);
});
