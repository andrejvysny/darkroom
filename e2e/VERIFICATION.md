# Darkroom — Tier-3 functional verification

Manual/automated verification driving the **real app + real Rust backend** over the
`tauri-plugin-playwright` socket bridge (Tier 3). Date: 2026-06-18. Catalog: 422 real Canon R7 CR3,
152 analyzed. All mutations capture→change→assert→**restore** so the catalog is left as found.

Run: app up via `npm run tauri -- dev --features e2e-testing`, then
`cd e2e && ../node_modules/.bin/playwright test --project=tauri --workers=1`.

## Result: 39/39 green — every exercised function works correctly

| Area                 | Commands / flows exercised                                                                                                                                                                                         | Verdict    |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------- |
| Library query        | count, query, sort (filename asc/desc reverse-equal, capture asc/desc monotonic, rating), paging (no overlap), search (filename/camera/no-match), folders (counts partition total), image_meta (+null for missing) | ✅ correct |
| Culling              | cull_set_rating/flag/label (single + `_many`); filters minStars / flag / colorLabel (+`__none__` sentinel) reflect changes                                                                                         | ✅ correct |
| Keywords             | add_to_image, add_to_images, keywords_for_image, list (computed count), filter by keywordId, remove, delete                                                                                                        | ✅ correct |
| Collections          | create (static+smart), add/remove images, filter by collectionId, live count, rename, delete; smart predicate stored                                                                                               | ✅ correct |
| Develop              | get/set edit round-trip + editedAt; GPU render (preview); **histogram tracks exposure**; preview/loupe JPEG; regen edited thumb; reset                                                                             | ✅ correct |
| Export               | export_image full-res JPEG (4640×6960, 7.9 MB) + PNG (51 MB), EXIF orientation applied                                                                                                                             | ✅ correct |
| Settings             | thumb_cache_cap/size, set cap (persist+restore); analysis_detector_size get/set                                                                                                                                    | ✅ correct |
| AI (read-only)       | analysis_status (coherent, idle); **facets == detectedCategory filter counts**; detections/caption/presence read                                                                                                   | ✅ correct |
| User labels          | set_image_user_label (+ `_many`), tri-state, image_user_labels                                                                                                                                                     | ✅ correct |
| Dedup (scan only)    | dedup_scan byte + capture return well-formed groups                                                                                                                                                                | ✅ correct |
| UI wiring (real DOM) | search filters grid (photo-count); thumbnail click → right panel; **star click → rating persists to DB**; double-click → loupe; Escape → grid; toolbar → Develop view                                              | ✅ correct |

## Findings

### F1 — Library right-panel histogram is a static decorative placeholder (cosmetic, misleading)

`RightInfo.tsx` `HistogramSvg()` draws a **procedural sine/cosine shape with fixed seeds** — it takes
no image data, never calls `develop_get_histogram`, and renders an **identical curve for every
image**. So the histogram shown in the Library info panel does not represent the selected photo.
(The **Develop**-view histogram is real and correct — verified it tracks exposure.) Low severity but
misleading; either wire it to real per-image data or mark it clearly as decorative.

### Non-issues confirmed (by design / correct behavior)

- **`keyword_add_to_image(s)` returns `count: 0`** — intentional (keywords.rs:14; count is computed
  only in `keywords_list`). Frontend uses only `id`/`name` and refreshes counts separately. ✅
- **`develop_render` skips the histogram for superseded requests** (commands.rs:667) using a
  session-monotonic `latest_render` atomic — a deliberate, correct guard against a slow earlier
  render clobbering the live histogram. (Callers must use increasing `request_id`s.) ✅
- **No "clear edit" command** — Reset writes default params, so `editedAt` stays set after a reset
  (matches the in-app Reset button). Minor product behavior, not a bug.

### Data observations (not bugs)

- 4 duplicate filenames exist (2 copies each: `_55A0460/0461/0468/0469.CR3`).
- Folder rows: `library` holds 421, `library/2026` holds 1, dated/`animals`/`nature` folders hold 0
  (counts still correctly partition the 422 total).

## Excluded per request (performance-heavy or destructive)

`library_index_root`, `database_reset`, `analysis_models_ensure`, `analysis_run`, `analysis_cancel`,
`features_backfill`, `dedup_scan_perceptual` (similarity), `dedup_resolve`/`_bulk` (trashes files),
`import_start` (filesystem mutation).

## Harness gotchas (test-side mistakes hit & fixed — useful for future Tier-3 work)

- `innerText` returns CSS `text-transform`ed text (headers come back UPPERCASE) — anchor on
  non-transformed text or compare case-insensitively.
- `SVGElement` has no `.click()` — drive SVG controls via `boundingBox` + `mouse.click(x,y)`.
- The grid is **virtualized** — a target cell only exists in the DOM when on screen; filter to it first.
- `develop_render` `request_id` must exceed the session's `latest_render`; use a timestamp base.
