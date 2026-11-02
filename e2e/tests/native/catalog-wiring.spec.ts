import { call } from "../../helpers";
import { test, expect, verifyProfile } from "./baseline-helpers";

test.skip(process.env.DARKROOM_NATIVE_CATALOG !== "1", "Explicit copied-profile opt-in required");
test("legacy catalog commands share the extracted runtime", async ({ tauriPage }, info) => {
  test.skip(info.project.name !== "tauri", "Requires real backend");
  await verifyProfile(tauriPage);
  type Row = { id: number; path: string; filename: string };
  const params = { includePaired: true, limit: 5000 };
  const rows = await call<Row[]>(tauriPage, "library_query", { params });
  const count = await call<number>(tauriPage, "library_count", { params });
  expect(rows.length).toBeGreaterThan(0);
  expect(count).toBe(rows.length);
  const row = rows.find((item) => item.filename === "reference.CR3");
  expect(row).toBeDefined();
  expect(await call<Row>(tauriPage, "image_meta", { id: row!.id })).toEqual(row);
  expect(await call(tauriPage, "image_meta", { id: -1 })).toBeNull();
  expect((await call<unknown[]>(tauriPage, "library_folders")).length).toBeGreaterThan(0);
  expect((await call<unknown[]>(tauriPage, "library_date_tree")).length).toBeGreaterThan(0);
  expect(await call(tauriPage, "library_query", { params: { search: "not-a-photo-135" } })).toEqual([]);
  expect(await call(tauriPage, "library_count", { params: { search: "not-a-photo-135" } })).toBe(0);
});

test("viewport admission survives a restarted client sequence", async ({ tauriPage }, info) => {
  test.skip(info.project.name !== "tauri", "Requires real backend");
  test.setTimeout(60000);
  await verifyProfile(tauriPage);
  const sizes = await tauriPage.evaluate<number[]>(`(async () => {
    const invoke = window.__TAURI__.core.invoke;
    const rows = await invoke('library_query', { params: { search: 'reference.CR3', limit: 100 } });
    const imageId = rows.find(row => row.filename === 'reference.CR3').id;
    const params = await invoke('develop_get_edit', { imageId });
    const sizes = [];
    for (const requestId of [1000000, 1]) {
      const frame = await invoke('develop_render', { imageId, params,
        view: { ox: 0, oy: 0, sx: 1, sy: 1 }, outW: 32, outH: 32,
        overlayMaskIndex: -1, cropPreview: false, requestId });
      sizes.push(frame.byteLength);
    }
    return sizes;
  })()`);
  expect(sizes).toEqual([32 * 32 * 4 + 9, 32 * 32 * 4 + 9]);
});
