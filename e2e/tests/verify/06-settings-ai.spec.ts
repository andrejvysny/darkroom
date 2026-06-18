import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

const X = 378,
  Y = 376,
  Z = 387;
const DET = 140; // recon: has People + Vehicles detections

type Status = {
  total: number;
  analyzed: number;
  pending: number;
  modelsReady: boolean;
  running: boolean;
};
type Facet = { category: string; count: number };
type Detection = {
  label: string;
  category: string;
  confidence: number;
  bbox: number[];
};
type Presence = { pPerson: number; pAnimal: number } | null;
type UserLabels = {
  containsPerson: boolean | null;
  containsAnimal: boolean | null;
};

// ── Settings ──────────────────────────────────────────────────────────────
test("thumb cache: cap/size read; set cap persists; restore", async ({
  tauriPage,
}) => {
  const orig = await call<number>(tauriPage, "thumb_cache_cap");
  expect(orig).toBeGreaterThan(0);
  expect(
    await call<number>(tauriPage, "thumb_cache_size"),
  ).toBeGreaterThanOrEqual(0);

  const bigger = orig + 4096; // larger ⇒ never evicts (safe)
  const freed = await call<number>(tauriPage, "set_thumb_cache_cap", {
    bytes: bigger,
  });
  expect(freed).toBeGreaterThanOrEqual(0);
  expect(await call<number>(tauriPage, "thumb_cache_cap")).toBe(bigger);

  await call(tauriPage, "set_thumb_cache_cap", { bytes: orig });
  expect(await call<number>(tauriPage, "thumb_cache_cap")).toBe(orig);
});

test("detector size: read, change, restore", async ({ tauriPage }) => {
  const orig = await call<number>(tauriPage, "analysis_detector_size");
  expect([640, 1280]).toContain(orig);
  const other = orig === 1280 ? 640 : 1280;
  await call(tauriPage, "set_analysis_detector_size", { size: other });
  expect(await call<number>(tauriPage, "analysis_detector_size")).toBe(other);
  await call(tauriPage, "set_analysis_detector_size", { size: orig });
  expect(await call<number>(tauriPage, "analysis_detector_size")).toBe(orig);
});

// ── AI read-only (no scan triggered) ────────────────────────────────────────
test("analysis status is coherent and idle", async ({ tauriPage }) => {
  const s = await call<Status>(tauriPage, "analysis_status");
  expect(s.running).toBe(false);
  expect(s.analyzed + s.pending).toBe(s.total);
  expect(s.total).toBe(
    await call<number>(tauriPage, "library_count", { params: {} }),
  );
});

test("facets match the detectedCategory filter counts", async ({
  tauriPage,
}) => {
  const facets = await call<Facet[]>(tauriPage, "analysis_facets");
  expect(facets.length).toBeGreaterThan(0);
  for (const f of facets) {
    const cnt = await call<number>(tauriPage, "library_count", {
      params: { detectedCategory: f.category },
    });
    expect(cnt).toBe(f.count);
  }
});

test("detections / caption / presence read for an analyzed image", async ({
  tauriPage,
}) => {
  const dets = await call<Detection[]>(tauriPage, "image_detections", {
    id: DET,
  });
  expect(dets.length).toBeGreaterThan(0);
  for (const d of dets) {
    expect(typeof d.label).toBe("string");
    expect(d.bbox.length).toBe(4);
    expect(d.confidence).toBeGreaterThan(0);
  }
  const cap = await call<{ caption: string } | null>(
    tauriPage,
    "image_caption",
    {
      id: DET,
    },
  );
  expect(cap === null || typeof cap.caption === "string").toBe(true);
  const pres = await call<Presence>(tauriPage, "image_presence", { id: DET });
  if (pres) {
    expect(pres.pPerson).toBeGreaterThanOrEqual(0);
    expect(pres.pPerson).toBeLessThanOrEqual(1);
  }
});

// ── Manual user labels (mutate + restore) ───────────────────────────────────
test("user labels: set single + many, restore to null", async ({
  tauriPage,
}) => {
  const orig = await call<UserLabels>(tauriPage, "image_user_labels", {
    id: X,
  });

  await call(tauriPage, "set_image_user_label", {
    id: X,
    field: "person",
    value: true,
  });
  await call(tauriPage, "set_image_user_label", {
    id: X,
    field: "animal",
    value: false,
  });
  let l = await call<UserLabels>(tauriPage, "image_user_labels", { id: X });
  expect(l.containsPerson).toBe(true);
  expect(l.containsAnimal).toBe(false);

  await call(tauriPage, "set_image_user_label_many", {
    imageIds: [Y, Z],
    field: "person",
    value: true,
  });
  for (const id of [Y, Z]) {
    l = await call<UserLabels>(tauriPage, "image_user_labels", { id });
    expect(l.containsPerson).toBe(true);
  }

  // restore
  await call(tauriPage, "set_image_user_label", {
    id: X,
    field: "person",
    value: orig.containsPerson,
  });
  await call(tauriPage, "set_image_user_label", {
    id: X,
    field: "animal",
    value: orig.containsAnimal,
  });
  await call(tauriPage, "set_image_user_label_many", {
    imageIds: [Y, Z],
    field: "person",
    value: null,
  });
  l = await call<UserLabels>(tauriPage, "image_user_labels", { id: X });
  expect(l.containsPerson).toBe(orig.containsPerson);
});

// ── Dedup scan (read-only; resolve NOT called) ──────────────────────────────
test("dedup scan byte + capture return well-formed groups", async ({
  tauriPage,
}) => {
  type DupGroup = {
    key: string;
    category: string;
    images: { id: number }[];
  };
  for (const category of ["byte", "capture"] as const) {
    const groups = await call<DupGroup[]>(tauriPage, "dedup_scan", {
      category,
    });
    expect(Array.isArray(groups)).toBe(true);
    for (const g of groups) {
      expect(typeof g.key).toBe("string");
      expect(g.images.length).toBeGreaterThanOrEqual(2);
    }
  }
});
