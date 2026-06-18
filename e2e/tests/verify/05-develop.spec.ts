import { test, expect } from "../../fixtures";
import { call, callLen } from "../../helpers";

// Develop target — id 378 was clean (no edit) at recon. NOTE: there is no "clear edit" command,
// so after these tests image 378 carries a default (no-op) edit, exactly like pressing Reset in UI.
const D = 378;

const HSL = Array.from({ length: 8 }, () => ({ h: 0, s: 0, l: 0 }));
function params(over: Record<string, number> = {}): Record<string, unknown> {
  return {
    exposure: 0,
    temp: 0,
    tint: 0,
    contrast: 0,
    saturation: 0,
    highlights: 0,
    shadows: 0,
    blacks: 0,
    whites: 0,
    sharpen: 0,
    nrLuma: 0,
    nrColor: 0,
    vignette: 0,
    toneCurve: { rgb: [], r: [], g: [], b: [] },
    hsl: HSL,
    masks: [],
    ...over,
  };
}

type Dev = { exposure: number; contrast: number };
type Hist = { r: number[]; g: number[]; b: number[] };
type Meta = { editedAt: number | null };

test("develop get/set edit round-trips and sets editedAt", async ({
  tauriPage,
}) => {
  const got = await call<Dev>(tauriPage, "develop_get_edit", { imageId: D });
  expect(typeof got.exposure).toBe("number");

  await call(tauriPage, "develop_set_edit", {
    imageId: D,
    params: params({ exposure: 0.75, contrast: 20 }),
  });
  const got2 = await call<Dev>(tauriPage, "develop_get_edit", { imageId: D });
  expect(got2.exposure).toBeCloseTo(0.75);
  expect(got2.contrast).toBe(20);

  const meta = await call<Meta>(tauriPage, "image_meta", { id: D });
  expect(meta.editedAt).not.toBeNull();
});

test("develop render pipeline runs and histogram tracks exposure", async ({
  tauriPage,
}) => {
  // request_id must exceed the app's session-monotonic latest_render (which persists across runs),
  // or the backend treats this render as superseded and skips committing its histogram. Use a
  // timestamp base so IDs are always increasing and far above any in-app counter.
  const base = Date.now();
  const dark = await callLen(tauriPage, "develop_render", {
    imageId: D,
    params: params({ exposure: -3 }),
    requestId: base,
    fullRes: false,
  });
  expect(dark).toBeGreaterThan(0);
  const hDark = await call<Hist | null>(tauriPage, "develop_get_histogram");

  const bright = await callLen(tauriPage, "develop_render", {
    imageId: D,
    params: params({ exposure: 3 }),
    requestId: base + 1,
    fullRes: false,
  });
  expect(bright).toBeGreaterThan(0);
  const hBright = await call<Hist | null>(tauriPage, "develop_get_histogram");

  expect(hDark).not.toBeNull();
  expect(hBright).not.toBeNull();
  expect(hDark!.r.length).toBe(256);
  const mean = (h: Hist) => {
    let s = 0,
      w = 0;
    for (let i = 0; i < h.r.length; i++) {
      s += i * h.r[i];
      w += h.r[i];
    }
    return w ? s / w : 0;
  };
  // Brighter exposure must push the histogram mass to higher bins.
  expect(mean(hBright!)).toBeGreaterThan(mean(hDark!));
});

test("preview + loupe JPEG return image bytes", async ({ tauriPage }) => {
  expect(
    await callLen(tauriPage, "develop_preview_jpeg", { imageId: D }),
  ).toBeGreaterThan(0);
  expect(
    await callLen(tauriPage, "loupe_jpeg", { imageId: D, maxEdge: 1024 }),
  ).toBeGreaterThan(0);
});

test("regen edited thumbnail returns a version (or null)", async ({
  tauriPage,
}) => {
  const v = await call<number | null>(tauriPage, "develop_regen_thumb", {
    imageId: D,
  });
  expect(v === null || typeof v === "number").toBe(true);
});

test("export full-res JPEG + PNG to disk", async ({ tauriPage }) => {
  await call(tauriPage, "export_image", {
    imageId: D,
    params: params({ exposure: 0.2 }),
    format: "jpeg",
    dest: "/tmp/dr-export.jpg",
  });
  await call(tauriPage, "export_image", {
    imageId: D,
    params: params(),
    format: "png",
    dest: "/tmp/dr-export.png",
  });
});

test("restore: reset edit on target image to defaults", async ({
  tauriPage,
}) => {
  await call(tauriPage, "develop_set_edit", {
    imageId: D,
    params: params(),
    force: true,
  });
  const got = await call<Dev>(tauriPage, "develop_get_edit", { imageId: D });
  expect(got.exposure).toBe(0);
});
