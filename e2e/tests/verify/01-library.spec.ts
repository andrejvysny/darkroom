import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

type Row = {
  id: number;
  filename: string;
  stars: number;
  flag: string;
  colorLabel: string | null;
  captureDate: number | null;
  cameraModel: string | null;
};
const q = (
  p: Record<string, unknown>,
  page: { evaluate<R>(s: string): Promise<R> },
) => call<Row[]>(page, "library_query", { params: p });
const c = (
  p: Record<string, unknown>,
  page: { evaluate<R>(s: string): Promise<R> },
) => call<number>(page, "library_count", { params: p });

test("count > 0 and query returns well-formed rows", async ({ tauriPage }) => {
  const total = await c({}, tauriPage);
  expect(total).toBeGreaterThan(0);
  const rows = await q({ limit: 5 }, tauriPage);
  expect(rows.length).toBe(5);
  for (const r of rows) {
    expect(typeof r.id).toBe("number");
    expect(typeof r.filename).toBe("string");
    expect(["none", "pick", "reject"]).toContain(r.flag);
  }
});

test("filename sort is monotonic asc & desc; desc reverses asc (full set)", async ({
  tauriPage,
}) => {
  const asc = (await q({ sort: "filename", limit: 2000 }, tauriPage)).map(
    (r) => r.filename,
  );
  const desc = (await q({ sort: "filename_desc", limit: 2000 }, tauriPage)).map(
    (r) => r.filename,
  );
  for (let i = 1; i < asc.length; i++) expect(asc[i] >= asc[i - 1]).toBe(true);
  for (let i = 1; i < desc.length; i++)
    expect(desc[i] <= desc[i - 1]).toBe(true);
  expect(desc).toEqual([...asc].reverse());
});

test("capture sort is monotonic both directions", async ({ tauriPage }) => {
  const desc = await q({ sort: "capture_desc", limit: 30 }, tauriPage);
  for (let i = 1; i < desc.length; i++)
    expect((desc[i].captureDate ?? 0) <= (desc[i - 1].captureDate ?? 0)).toBe(
      true,
    );
  const asc = await q({ sort: "capture_asc", limit: 30 }, tauriPage);
  for (let i = 1; i < asc.length; i++)
    expect((asc[i].captureDate ?? 0) >= (asc[i - 1].captureDate ?? 0)).toBe(
      true,
    );
});

test("paging windows do not overlap and respect limit", async ({
  tauriPage,
}) => {
  const p1 = await q({ sort: "filename", limit: 5, offset: 0 }, tauriPage);
  const p2 = await q({ sort: "filename", limit: 5, offset: 5 }, tauriPage);
  expect(p1.length).toBe(5);
  expect(p2.length).toBe(5);
  const ids1 = new Set(p1.map((r) => r.id));
  expect(p2.some((r) => ids1.has(r.id))).toBe(false);
});

test("search: filename substring, camera, and no-match", async ({
  tauriPage,
}) => {
  const first = (await q({ limit: 1, sort: "filename" }, tauriPage))[0];
  const part = first.filename.slice(0, 6);
  const byName = await q({ search: part, limit: 100 }, tauriPage);
  expect(byName.length).toBeGreaterThan(0);
  const byCam = await c({ search: "R7" }, tauriPage); // all are "EOS R7"
  expect(byCam).toBeGreaterThan(0);
  const none = await c({ search: "zzqq_no_such_token_42" }, tauriPage);
  expect(none).toBe(0);
});

test("folder counts partition the library; folder filter matches", async ({
  tauriPage,
}) => {
  const total = await c({}, tauriPage);
  const folders = await call<{ id: number; count: number }[]>(
    tauriPage,
    "library_folders",
  );
  expect(folders.reduce((a, f) => a + f.count, 0)).toBe(total);
  const biggest = [...folders].sort((a, b) => b.count - a.count)[0];
  expect(await c({ folderId: biggest.id }, tauriPage)).toBe(biggest.count);
});

test("image_meta resolves a real id and returns null for a missing one", async ({
  tauriPage,
}) => {
  const r0 = (await q({ limit: 1 }, tauriPage))[0];
  const m = await call<Row | null>(tauriPage, "image_meta", { id: r0.id });
  expect(m?.id).toBe(r0.id);
  expect(m?.filename).toBe(r0.filename);
  const missing = await call<Row | null>(tauriPage, "image_meta", {
    id: 99999999,
  });
  expect(missing).toBeNull();
});
