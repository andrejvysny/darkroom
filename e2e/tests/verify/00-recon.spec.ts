import { writeFileSync } from "node:fs";
import { test, expect } from "../../fixtures";
import { call } from "../../helpers";

// Recon pass — capture real catalog state so later checks use precise, restorable targets.
test("recon: capture catalog state → /tmp/dr-recon.json", async ({
  tauriPage,
}) => {
  const out: Record<string, unknown> = {};
  out.total = await call(tauriPage, "library_count", { params: {} });
  out.rows = await call(tauriPage, "library_query", {
    params: { limit: 12, sort: "filename" },
  });
  out.folders = await call(tauriPage, "library_folders");
  out.keywords = await call(tauriPage, "keywords_list");
  out.collections = await call(tauriPage, "collections_list");
  out.analysisStatus = await call(tauriPage, "analysis_status");
  out.facets = await call(tauriPage, "analysis_facets");
  out.detectorSize = await call(tauriPage, "analysis_detector_size");
  out.thumbCap = await call(tauriPage, "thumb_cache_cap");
  out.thumbSize = await call(tauriPage, "thumb_cache_size");
  out.libraryRoot = await call(tauriPage, "app_library_root");
  out.defaultLibrary = await call(tauriPage, "app_default_library");

  const detected: Record<string, unknown> = {};
  for (const cat of ["People", "Animals", "Vehicles"]) {
    detected[cat] = await call(tauriPage, "library_query", {
      params: { detectedCategory: cat, limit: 2 },
    });
  }
  out.detected = detected;

  writeFileSync("/tmp/dr-recon.json", JSON.stringify(out, null, 2));
  expect(out.total).toBeGreaterThan(0);
});
