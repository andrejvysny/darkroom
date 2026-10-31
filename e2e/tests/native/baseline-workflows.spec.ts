import { stat, writeFile } from "node:fs/promises";
import { call } from "../../helpers";
import { test, expect, installCapture, mark, rss, setExposure, uninstallCapture, waitFrame, verifyProfile, resetCopiedEdit } from "./baseline-helpers";
import type { TauriPage } from "@srsholmes/tauri-playwright";

const PROFILE = "/private/tmp/darkroom-native-baseline-profile-20261030";
type Edit = { exposure: number; masks: { adjust: { exposure: number } }[] };

test.skip(process.env.DARKROOM_NATIVE_BASELINE !== "1", "Requires isolated native baseline");

test("measure masked editing during copied-source full-resolution export", async ({ tauriPage }, info) => {
  test.setTimeout(180_000);
  test.skip(info.project.name !== "tauri", "Requires native socket mode");
  await verifyProfile(tauriPage);
  const status = await call<{ directory: string }>(tauriPage, "logs_status");
  expect(status.directory.split("/")).toContain("com.andrejvysny.darkroom.baseline.20261030");
  const rows = await call<{ id: number; path: string }[]>(tauriPage, "library_query", { params: { search: "reference.CR3" } });
  const row = rows.find((entry) => entry.path === `${PROFILE}/photos/reference.CR3`);
  expect(row).toBeDefined();
  if (!row) throw new Error("Copied reference missing");
  await tauriPage.getByTestId("nav-library").click();
  await tauriPage.locator(`[data-image-id="${row.id}"]`).click();
  await installCapture(tauriPage);
  try {
    await mark(tauriPage);
    await tauriPage.getByTestId("nav-develop").click();
    await waitFrame(tauriPage);
    await resetCopiedEdit(tauriPage);
    const masked = await measureMask(tauriPage);
    const exported = await exportWhileEditing(tauriPage, row.id, `${PROFILE}/exports/baseline-${Date.now()}.jpg`);
    const resize = await resizeWindow(tauriPage);
    const path = info.outputPath("native-workflows.json");
    await writeFile(path, JSON.stringify({ masked, exported, resize, afterKiB: rss(),
      boundary: "Real native backend with synthetic pointer events and bypassed pointer capture; process RSS is not GPU residency." }, null, 2));
    await info.attach("native-workflows", { path, contentType: "application/json" });
  } finally {
    await uninstallCapture(tauriPage);
  }
});

async function measureMask(page: TauriPage) {
  const beforeKiB = rss();
  await mark(page);
  await page.evaluate(`(() => {
    const button = Array.from(document.querySelectorAll('button')).find(element => element.textContent.trim() === '+ Linear');
    if (!button) throw new Error('Linear mask action missing');
    button.click();
  })()`);
  const added = await waitFrame(page, 30000, { masks: [0] });
  await mark(page);
  await setExposure(page, 0.5);
  const adjusted = await waitFrame(page, 30000, { masks: [0.5] });
  const params = await page.evaluate<Edit>("(async () => (await import('/src/store/develop.ts')).useDevelopStore.getState().params)()");
  expect(params.masks.length).toBeGreaterThan(0);
  expect(params.masks[params.masks.length - 1].adjust.exposure).toBeCloseTo(0.5, 4);
  return { beforeKiB, afterKiB: rss(), added, adjusted, params };
}

async function exportWhileEditing(page: TauriPage, id: number, dest: string) {
  await page.evaluate(`(async () => {
    const params = (await import('/src/store/develop.ts')).useDevelopStore.getState().params;
    window.__baselineExport = { started: performance.now(), finished: null, error: null };
    window.__TAURI_INTERNALS__.invoke('export_image', { imageId: ${id}, params, format: 'jpeg', dest: ${JSON.stringify(dest)}, quality: 85 })
      .then(() => { window.__baselineExport.finished = performance.now(); })
      .catch(error => { window.__baselineExport.error = String(error); window.__baselineExport.finished = performance.now(); });
  })()`);
  const samples = [];
  const edits = [];
  const deadline = Date.now() + 90_000;
  let state: { started: number; finished: number | null; error: string | null };
  let i = 0;
  do {
    samples.push(rss());
    await mark(page);
    const value = i++ % 2 ? 0.5 : -0.5;
    await setExposure(page, value);
    edits.push(await waitFrame(page, 30000, { masks: [value] }));
    state = await page.evaluate("window.__baselineExport");
    if (Date.now() > deadline) throw new Error("Export did not finish within 90 seconds");
  } while (state.finished === null);
  expect(state.error).toBeNull();
  expect((await stat(dest)).size).toBeGreaterThan(1000);
  const saved = await call<Edit>(page, "develop_get_edit", { imageId: id });
  expect(saved.masks.length).toBeGreaterThan(0);
  return { dest, state, samplesKiB: samples, edits, outputBytes: (await stat(dest)).size };
}

async function resizeWindow(page: TauriPage) {
  const before = await page.evaluate<number[]>("[innerWidth, innerHeight]");
  const resized = await page.evaluate<string>(`(async () => {
    try { await window.__TAURI__.window.getCurrentWindow().setSize(new window.__TAURI__.dpi.LogicalSize(1180, 760)); return 'requested'; }
    catch (error) { return String(error); }
  })()`);
  if (resized !== "requested") return { before, status: `unavailable: ${resized}` };
  await page.waitForFunction("innerWidth === 1180 && innerHeight === 760");
  const after = await page.evaluate<number[]>("[innerWidth, innerHeight]");
  await page.evaluate(`window.__TAURI__.window.getCurrentWindow().setSize(new window.__TAURI__.dpi.LogicalSize(${before[0]}, ${before[1]}))`);
  return { before, after, status: "passed" };
}
