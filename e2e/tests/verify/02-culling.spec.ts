import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

type Row = {
  id: number;
  stars: number;
  flag: string;
  colorLabel: string | null;
};
const meta = (page: { evaluate<R>(s: string): Promise<R> }, id: number) =>
  call<Row>(page, "image_meta", { id });
const ids = (
  page: { evaluate<R>(s: string): Promise<R> },
  p: Record<string, unknown>,
) =>
  call<Row[]>(page, "library_query", { params: { ...p, limit: 1000 } }).then(
    (rs) => rs.map((r) => r.id),
  );

// All clean targets at recon time: stars 0 / flag none / label null.
const X = 378;
const BATCH = [376, 387, 388];

test("rating: set, filter by minStars, restore", async ({ tauriPage }) => {
  const orig = await meta(tauriPage, X);
  await call(tauriPage, "cull_set_rating", { imageId: X, stars: 3 });
  expect((await meta(tauriPage, X)).stars).toBe(3);
  expect(await ids(tauriPage, { minStars: 3 })).toContain(X);
  expect(await ids(tauriPage, { minStars: 4 })).not.toContain(X);
  await call(tauriPage, "cull_set_rating", { imageId: X, stars: orig.stars });
  expect((await meta(tauriPage, X)).stars).toBe(orig.stars);
});

test("flag: pick/reject, filter by flag, restore", async ({ tauriPage }) => {
  const orig = await meta(tauriPage, X);
  await call(tauriPage, "cull_set_flag", { imageId: X, flag: "pick" });
  expect((await meta(tauriPage, X)).flag).toBe("pick");
  expect(await ids(tauriPage, { flag: "pick" })).toContain(X);
  expect(await ids(tauriPage, { flag: "reject" })).not.toContain(X);
  await call(tauriPage, "cull_set_flag", { imageId: X, flag: "reject" });
  expect(await ids(tauriPage, { flag: "reject" })).toContain(X);
  await call(tauriPage, "cull_set_flag", { imageId: X, flag: orig.flag });
  expect((await meta(tauriPage, X)).flag).toBe(orig.flag);
});

test("color label: set, filter by label + __none__ sentinel, restore", async ({
  tauriPage,
}) => {
  const orig = await meta(tauriPage, X);
  await call(tauriPage, "cull_set_label", { imageId: X, label: "red" });
  expect((await meta(tauriPage, X)).colorLabel).toBe("red");
  expect(await ids(tauriPage, { colorLabel: "red" })).toContain(X);
  expect(await ids(tauriPage, { colorLabel: "__none__" })).not.toContain(X);
  await call(tauriPage, "cull_set_label", { imageId: X, label: null });
  expect((await meta(tauriPage, X)).colorLabel).toBeNull();
  expect(await ids(tauriPage, { colorLabel: "__none__" })).toContain(X);
});

test("batch culling: set many, verify each, restore", async ({ tauriPage }) => {
  await call(tauriPage, "cull_set_rating_many", { imageIds: BATCH, stars: 2 });
  await call(tauriPage, "cull_set_flag_many", {
    imageIds: BATCH,
    flag: "pick",
  });
  await call(tauriPage, "cull_set_label_many", {
    imageIds: BATCH,
    label: "blue",
  });
  for (const id of BATCH) {
    const m = await meta(tauriPage, id);
    expect(m.stars).toBe(2);
    expect(m.flag).toBe("pick");
    expect(m.colorLabel).toBe("blue");
  }
  // restore
  await call(tauriPage, "cull_set_rating_many", { imageIds: BATCH, stars: 0 });
  await call(tauriPage, "cull_set_flag_many", {
    imageIds: BATCH,
    flag: "none",
  });
  await call(tauriPage, "cull_set_label_many", {
    imageIds: BATCH,
    label: null,
  });
  for (const id of BATCH) {
    const m = await meta(tauriPage, id);
    expect(m.stars).toBe(0);
    expect(m.flag).toBe("none");
    expect(m.colorLabel).toBeNull();
  }
});
