import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

type Kw = { id: number; name: string; count: number };
type Row = { id: number };
const X = 378,
  Y = 376,
  Z = 387;

// Note: keyword_add_to_image(s) intentionally returns count:0 (keywords.rs:14 — count is computed
// only in keywords_list). So we verify tagging via keywords_for_image / filter / list counts.

test("keyword: add to one image, list, filter, remove, delete", async ({
  tauriPage,
}) => {
  const name = "e2e-kw-alpha";
  const row = await call<Kw>(tauriPage, "keyword_add_to_image", {
    imageId: X,
    name,
  });
  expect(row.name).toBe(name);

  expect(
    (await call<Kw[]>(tauriPage, "keywords_for_image", { imageId: X })).some(
      (k) => k.id === row.id,
    ),
  ).toBe(true);
  expect(
    (
      await call<Row[]>(tauriPage, "library_query", {
        params: { keywordId: row.id, limit: 1000 },
      })
    ).map((r) => r.id),
  ).toContain(X);
  // computed count lives in keywords_list
  expect(
    (await call<Kw[]>(tauriPage, "keywords_list")).find((k) => k.id === row.id)
      ?.count,
  ).toBe(1);

  await call(tauriPage, "keyword_remove_from_image", {
    imageId: X,
    keywordId: row.id,
  });
  expect(
    (await call<Kw[]>(tauriPage, "keywords_for_image", { imageId: X })).some(
      (k) => k.id === row.id,
    ),
  ).toBe(false);

  await call(tauriPage, "keyword_delete", { keywordId: row.id });
  expect(
    (await call<Kw[]>(tauriPage, "keywords_list")).some((k) => k.id === row.id),
  ).toBe(false);
});

test("keyword: add to many → list count 3, tagged on each, cleanup", async ({
  tauriPage,
}) => {
  const name = "e2e-kw-beta";
  const row = await call<Kw>(tauriPage, "keyword_add_to_images", {
    imageIds: [X, Y, Z],
    name,
  });
  expect(row.name).toBe(name);

  for (const id of [X, Y, Z]) {
    expect(
      (await call<Kw[]>(tauriPage, "keywords_for_image", { imageId: id })).some(
        (k) => k.id === row.id,
      ),
    ).toBe(true);
  }
  expect(
    await call<number>(tauriPage, "library_count", {
      params: { keywordId: row.id },
    }),
  ).toBe(3);
  expect(
    (await call<Kw[]>(tauriPage, "keywords_list")).find((k) => k.id === row.id)
      ?.count,
  ).toBe(3);

  await call(tauriPage, "keyword_delete", { keywordId: row.id });
  expect(
    (await call<Kw[]>(tauriPage, "keywords_list")).some((k) => k.id === row.id),
  ).toBe(false);
  for (const id of [X, Y, Z]) {
    expect(
      (await call<Kw[]>(tauriPage, "keywords_for_image", { imageId: id })).some(
        (k) => k.id === row.id,
      ),
    ).toBe(false);
  }
});
