import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

type Coll = {
  id: number;
  name: string;
  isSmart: boolean;
  query: string | null;
  count: number;
};
type Row = { id: number };
const X = 378,
  Y = 376;

test("static collection: create→add→filter→remove→rename→delete", async ({
  tauriPage,
}) => {
  const id = await call<number>(tauriPage, "collection_create", {
    name: "e2e-coll",
    isSmart: false,
    query: null,
  });
  expect(typeof id).toBe("number");

  let created = (await call<Coll[]>(tauriPage, "collections_list")).find(
    (c) => c.id === id,
  );
  expect(created?.isSmart).toBe(false);
  expect(created?.count).toBe(0);

  const added = await call<number>(tauriPage, "collection_add_images", {
    collectionId: id,
    imageIds: [X, Y],
  });
  expect(added).toBe(2);
  expect(
    (
      await call<Coll[]>(tauriPage, "collections_for_image", { imageId: X })
    ).some((c) => c.id === id),
  ).toBe(true);

  const filtered = await call<Row[]>(tauriPage, "library_query", {
    params: { collectionId: id, limit: 1000 },
  });
  expect(filtered.map((r) => r.id).sort()).toEqual([X, Y].sort());

  created = (await call<Coll[]>(tauriPage, "collections_list")).find(
    (c) => c.id === id,
  );
  expect(created?.count).toBe(2);

  const removed = await call<number>(tauriPage, "collection_remove_images", {
    collectionId: id,
    imageIds: [X],
  });
  expect(removed).toBe(1);
  expect(
    await call<number>(tauriPage, "library_count", {
      params: { collectionId: id },
    }),
  ).toBe(1);

  await call(tauriPage, "collection_rename", { id, name: "e2e-coll-renamed" });
  expect(
    (await call<Coll[]>(tauriPage, "collections_list")).find((c) => c.id === id)
      ?.name,
  ).toBe("e2e-coll-renamed");

  await call(tauriPage, "collection_delete", { id });
  expect(
    (await call<Coll[]>(tauriPage, "collections_list")).some(
      (c) => c.id === id,
    ),
  ).toBe(false);
});

test("smart collection stores its predicate JSON", async ({ tauriPage }) => {
  const query = JSON.stringify({ minStars: 1 });
  const id = await call<number>(tauriPage, "collection_create", {
    name: "e2e-smart",
    isSmart: true,
    query,
  });
  const c = (await call<Coll[]>(tauriPage, "collections_list")).find(
    (c) => c.id === id,
  );
  expect(c?.isSmart).toBe(true);
  expect(c?.query).toBe(query);
  await call(tauriPage, "collection_delete", { id });
});
