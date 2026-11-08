import { writeFile } from "node:fs/promises";
import { call } from "../../helpers";
import type { TauriPage as TP } from "@srsholmes/tauri-playwright";
import { test, expect, installCapture, mark, rss, setExposure, uninstallCapture, waitFrame, verifyProfile, resetCopiedEdit } from "./baseline-helpers";

const PROFILE = "/private/tmp/darkroom-native-baseline-profile-20261030";
const PHOTO = `${PROFILE}/photos/reference.CR3`;
type Row = { id: number; path: string; stars: number; filename: string };
type Status = { directory: string };
type Gpu = { backend: string; name: string };

test.skip(process.env.DARKROOM_NATIVE_BASELINE !== "1", "Native baseline requires explicit opt-in");

test("measure copied-profile native develop baseline", async ({ tauriPage }, testInfo) => {
  test.skip(testInfo.project.name !== "tauri", "Requires real Tauri mode");
  test.setTimeout(300_000);
  await verifyProfile(tauriPage);
  const status = await call<Status>(tauriPage, "logs_status");
  expect(status.directory.split("/")).toContain("com.andrejvysny.darkroom.baseline.20261030");
  const environment = await tauriPage.evaluate<{ mock: string; viewport: number[]; dpr: number; userAgent: string }>(
    "({ mock: typeof window.__darkroomThumbMock, viewport: [innerWidth, innerHeight], dpr: devicePixelRatio, userAgent: navigator.userAgent })",
  );
  expect(environment.mock).toBe("undefined");
  const gpu = await call<Gpu | null>(tauriPage, "gpu_status");
  expect(gpu?.backend.toLowerCase()).toBe("metal");
  const memory: Record<string, number | null> = { idleKiB: rss() };
  const index = await call(tauriPage, "library_index_root", { path: `${PROFILE}/photos` });
  const rows = await call<Row[]>(tauriPage, "library_query", { params: { search: "reference.CR3", limit: 100 } });
  const matches = rows.filter((row) => row.path === PHOTO);
  expect(matches).toHaveLength(1);
  const row = matches[0];
  const meta = await call<Row>(tauriPage, "image_meta", { id: row.id });
  expect(meta.path).toBe(PHOTO);
  await seedSelection(tauriPage, row);
  await installCapture(tauriPage);
  try {
    await captureBaseline(tauriPage, testInfo, { status, environment, gpu, memory, index, row, meta });
  } finally { await uninstallCapture(tauriPage); }
});


type BaselineContext = {
  status: Status; environment: { mock: string; viewport: number[]; dpr: number; userAgent: string };
  gpu: Gpu | null; memory: Record<string, number | null>; index: unknown; row: Row; meta: Row;
};

async function captureBaseline(page: TP, info: import("@playwright/test").TestInfo, context: BaselineContext): Promise<void> {
  await mark(page);
  await page.getByTestId("nav-develop").click();
  const firstNavigation = await waitFrame(page, 60000, { imageId: context.row.id });
  expect(firstNavigation.frames[0].width).toBeGreaterThan(0);
  expect(firstNavigation.frames[0].height).toBeGreaterThan(0);
  context.memory.readyKiB = rss();
  await resetCopiedEdit(page);
  const edits = await measureEdits(page);
  context.memory.afterEditsKiB = rss();
  await expect.poll(async () => (await call<{ exposure: number }>(page, "develop_get_edit", { imageId: context.row.id })).exposure).toBeCloseTo(-0.75, 4);
  const switches = await reopen(page, context.row.id);
  const restored = await page.evaluate<number>("(async () => (await import('/src/store/develop.ts')).useDevelopStore.getState().params.exposure)()");
  expect(restored).toBeCloseTo(-0.75, 4);
  await verifyCulling(page, context.row.id, context.meta.stars);
  const screenshot = await captureScreenshot(page, info);
  const result = { ...context, photo: PHOTO, firstNavigation, edits, switches, screenshot,
    measurement: "Bridge mark before synthetic React pointer event to Stage putImageData matched byte-for-byte to response pixels and matching immutable request parameters; firstNavigation is not OS-cache-cold; test-only pointer capture bypassed. Not physical input, compositor or photon time. RSS is instrumented process KiB, including up to eight retained response buffers; not GPU memory. Paint time precedes byte matching, but earlier instrumentation still perturbs the process.",
    build: process.env.DARKROOM_BASELINE_BUILD ?? "unspecified e2e-testing build" };
  const path = info.outputPath("native-baseline.json");
  await writeFile(path, JSON.stringify(result, null, 2));
  await info.attach("native-baseline", { path, contentType: "application/json" });
}

async function measureEdits(page: TP) {
  const edits = [];
  for (let i = 0; i < 30; i++) {
    const value = i % 2 === 0 ? 0.75 : -0.75;
    await mark(page);
    await setExposure(page, value);
    const capture = await waitFrame(page, 30000, { exposure: value, masks: [] });
    expect(capture.exposure).toBeCloseTo(value, 4);
    edits.push({ requested: value, ...capture });
  }
  return edits;
}

async function verifyCulling(page: TP, id: number, original: number): Promise<void> {
  await call(page, "cull_set_rating", { imageId: id, stars: 3 });
  try {
    expect((await call<Row>(page, "image_meta", { id })).stars).toBe(3);
  } finally {
    await call(page, "cull_set_rating", { imageId: id, stars: original });
  }
}

async function captureScreenshot(page: TP, info: import("@playwright/test").TestInfo): Promise<string> {
  try {
    const path = info.outputPath("native-window.png");
    await writeFile(path, Buffer.from(await page.screenshot()));
    await info.attach("native-window", { path, contentType: "image/png" });
    return "captured";
  } catch (error) { return `unavailable: ${String(error)}`; }
}

async function seedSelection(page: TP, row: Row): Promise<void> {
  await page.getByTestId("nav-library").click();
  await page.locator(`[data-image-id="${row.id}"]`).click();
  await page.waitForFunction(`document.querySelector('[data-image-id="${row.id}"]') !== null`);
  const selected = await page.evaluate<number>(
    "(async () => (await import('/src/store/app.ts')).useAppStore.getState().selectedId)()",
  );
  expect(selected).toBe(row.id);
}

async function reopen(page: TP, id: number) {
  const switches = [];
  for (let i = 0; i < 8; i++) {
    await page.getByTestId("nav-library").click();
    await mark(page);
    await page.getByTestId("nav-develop").click();
    const frame = await waitFrame(page, 60000, { imageId: id });
    switches.push(frame);
    expect(frame.frames[0].width).toBeGreaterThan(0);
    const selected = await page.evaluate<number>("(async () => (await import('/src/store/app.ts')).useAppStore.getState().selectedId)()");
    expect(selected).toBe(id);
  }
  return switches;
}
