import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";

// ── Types ──────────────────────────────────────────────────────────────────

export type SortKey =
  | "capture_desc"
  | "capture_asc"
  | "filename"
  | "filename_desc"
  | "rating_desc"
  | "rating_asc"
  | "imported_desc"
  | "imported_asc";

/** Sentinel `colorLabel` value that matches images with no color label. */
export const LABEL_NONE = "__none__";

export type QueryParams = {
  folderId?: number | null;
  minStars?: number | null;
  flag?: string | null;
  colorLabel?: string | null;
  keywordId?: number | null;
  collectionId?: number | null;
  importSessionId?: number | null;
  /** Detected-object bucket filter: "People" | "Animals" | "Vehicles". */
  detectedCategory?: string | null;
  /** Restrict to images containing a (confirmed or suggested) face of this person. */
  personId?: number | null;
  search?: string | null;
  sort?: SortKey;
  limit?: number;
  offset?: number;
};

/** The filter dimensions (excludes sort/search/paging) — the keys "All photos" clears. */
export const FILTER_DIMENSIONS: (keyof QueryParams)[] = [
  "folderId",
  "minStars",
  "flag",
  "colorLabel",
  "keywordId",
  "collectionId",
  "importSessionId",
  "detectedCategory",
  "personId",
];

/** True when any filter dimension is active. Single source of truth for nav/footer state. */
export function hasActiveFilters(p: QueryParams): boolean {
  return FILTER_DIMENSIONS.some((k) => p[k] != null);
}

/** A params patch that clears every filter dimension (keeps sort & search). */
export function clearedFilters(): Partial<QueryParams> {
  return {
    folderId: null,
    minStars: null,
    flag: null,
    colorLabel: null,
    keywordId: null,
    collectionId: null,
    importSessionId: null,
    detectedCategory: null,
    personId: null,
  };
}

export type ImageRow = {
  id: number;
  contentHash: string;
  path: string;
  filename: string;
  captureDate: number | null;
  cameraMake: string | null;
  cameraModel: string | null;
  lens: string | null;
  iso: number | null;
  shutter: string | null;
  aperture: number | null;
  focalLength: number | null;
  width: number | null;
  height: number | null;
  orientation: number | null;
  stars: number;
  flag: "none" | "pick" | "reject";
  colorLabel: string | null;
  /** `edits.updated_at` if the image has a develop edit; versions edit-aware previews (null = none). */
  editedAt: number | null;
};

export type FolderRow = {
  id: number;
  path: string;
  count: number;
};

export type IndexStats = {
  scanned: number;
  added: number;
  skipped: number;
  failed: number;
};

// ── IPC Wrappers ───────────────────────────────────────────────────────────

export function libraryQuery(params: QueryParams): Promise<ImageRow[]> {
  return invoke<ImageRow[]>("library_query", { params });
}

export function libraryCount(params: QueryParams): Promise<number> {
  return invoke<number>("library_count", { params });
}

export function libraryFolders(): Promise<FolderRow[]> {
  return invoke<FolderRow[]>("library_folders", {});
}

export function imageMeta(id: number): Promise<ImageRow | null> {
  return invoke<ImageRow | null>("image_meta", { id });
}

export function libraryIndexRoot(path: string): Promise<IndexStats> {
  return invoke<IndexStats>("library_index_root", { path });
}

/** Wipe the catalog (index/metadata/settings) and rebuild it from disk. Files on disk are never
 *  touched. Resolves with the aggregate re-index stats. */
export function databaseReset(): Promise<IndexStats> {
  return invoke<IndexStats>("database_reset", {});
}

export function appDefaultLibrary(): Promise<string | null> {
  return invoke<string | null>("app_default_library", {});
}

// ── Import / Dedup types ───────────────────────────────────────────────────

export type ImportMode = "copy" | "move" | "reference";

export type ImportStats = {
  sessionId: number;
  total: number;
  added: number;
  skipped: number;
  failed: number;
  /** Move-mode files catalogued but whose original could not be sent to Trash (source kept). */
  sourceRetained: number;
};

export type DupImage = {
  id: number;
  contentHash: string;
  path: string;
  filename: string;
  fileSize: number;
  captureDate: number | null;
};

export type DupGroup = {
  key: string;
  category: string;
  images: DupImage[];
};

// ── Import / Dedup IPC ─────────────────────────────────────────────────────

export function appLibraryRoot(): Promise<string | null> {
  return invoke<string | null>("app_library_root", {});
}

export function importStart(
  source: string,
  mode: ImportMode,
  dest: string,
  recursive = true,
): Promise<ImportStats> {
  return invoke<ImportStats>("import_start", { source, mode, dest, recursive });
}

export function dedupScan(category: "byte" | "capture"): Promise<DupGroup[]> {
  return invoke<DupGroup[]>("dedup_scan", { category });
}

/** Perceptual near-duplicate scan. `threshold` = max differing dHash bits (0–64; ~10 is typical).
 *  Lazily computes missing dHashes first (emits `dedup:progress`). */
export function dedupScanPerceptual(threshold: number): Promise<DupGroup[]> {
  return invoke<DupGroup[]>("dedup_scan_perceptual", { threshold });
}

export function dedupResolve(
  keepId: number,
  trashIds: number[],
  /** Decision context for the behavioral log (optional): the full group, the rule's suggested
   *  keeper, and the group key — lets us later learn keeper ranking + detect user overrides. */
  ctx?: { candidateIds?: number[]; autoKeeperId?: number; groupId?: string },
): Promise<number> {
  return invoke<number>("dedup_resolve", {
    keepId,
    trashIds,
    candidateIds: ctx?.candidateIds,
    autoKeeperId: ctx?.autoKeeperId,
    groupId: ctx?.groupId,
  });
}

/** Auto-resolve all byte-identical groups (keep one each, trash the rest). Resolves to count trashed. */
export function dedupResolveBulk(): Promise<number> {
  return invoke<number>("dedup_resolve_bulk", {});
}

// ── Settings ───────────────────────────────────────────────────────────────

/** Configured thumbnail-cache cap, in bytes. */
export function thumbCacheCap(): Promise<number> {
  return invoke<number>("thumb_cache_cap", {});
}

/** Current on-disk size of the thumbnail cache, in bytes. */
export function thumbCacheSize(): Promise<number> {
  return invoke<number>("thumb_cache_size", {});
}

/** Persist a new cap (bytes) and evict down to it. Resolves to bytes freed. */
export function setThumbCacheCap(bytes: number): Promise<number> {
  return invoke<number>("set_thumb_cache_cap", { bytes });
}

// ── Utilities ──────────────────────────────────────────────────────────────

export function thumbUrl(
  hash: string,
  size = 512,
  editedAt?: number | null,
): string {
  const base = `thumb://localhost/${hash}?size=${size}`;
  // `edit=<version>` makes the protocol serve the edited render and changes the URL on each edit
  // (cache-busting the immutable-cached `<img>`).
  const url = editedAt != null ? `${base}&edit=${editedAt}` : base;
  // Dev-only: in a plain browser the `thumb://` protocol has no handler. A mock backend
  // (src/dev/tauriMock.ts) installs `window.__darkroomThumbMock` to serve placeholder images.
  // Tree-shaken from production builds via the DEV guard; never set inside the Tauri shell.
  if (import.meta.env.DEV) {
    const mock = window.__darkroomThumbMock;
    if (mock) return mock(url);
  }
  return url;
}

/** Regenerate the edited thumbnail for an image (on edit-settle); emits `develop:edit-changed`. */
export function developRegenThumb(imageId: number): Promise<number | null> {
  return invoke<number | null>("develop_regen_thumb", { imageId });
}

// ── Cull IPC ───────────────────────────────────────────────────────────────

/** Optional decision context for the behavioral log (cheap implicit weights + within-group set). */
export type CullCtx = {
  latencyMs?: number;
  groupId?: string;
  candidateIds?: number[];
};

export function cullSetRating(
  imageId: number,
  stars: number,
  ctx?: CullCtx,
): Promise<void> {
  return invoke<void>("cull_set_rating", {
    imageId,
    stars,
    latencyMs: ctx?.latencyMs,
    groupId: ctx?.groupId,
    candidateIds: ctx?.candidateIds,
  });
}

export function cullSetFlag(
  imageId: number,
  flag: "none" | "pick" | "reject",
  ctx?: CullCtx,
): Promise<void> {
  return invoke<void>("cull_set_flag", {
    imageId,
    flag,
    latencyMs: ctx?.latencyMs,
    groupId: ctx?.groupId,
    candidateIds: ctx?.candidateIds,
  });
}

export function cullSetLabel(
  imageId: number,
  label: string | null,
  ctx?: CullCtx,
): Promise<void> {
  return invoke<void>("cull_set_label", {
    imageId,
    label,
    latencyMs: ctx?.latencyMs,
    groupId: ctx?.groupId,
  });
}

// Batch culling (apply one value to a whole selection). The selection is the candidate group.

export function cullSetRatingMany(
  imageIds: number[],
  stars: number,
  groupId?: string,
): Promise<void> {
  return invoke<void>("cull_set_rating_many", { imageIds, stars, groupId });
}

export function cullSetFlagMany(
  imageIds: number[],
  flag: "none" | "pick" | "reject",
  groupId?: string,
): Promise<void> {
  return invoke<void>("cull_set_flag_many", { imageIds, flag, groupId });
}

export function cullSetLabelMany(
  imageIds: number[],
  label: string | null,
  groupId?: string,
): Promise<void> {
  return invoke<void>("cull_set_label_many", { imageIds, label, groupId });
}

// ── Keywords / tags ──────────────────────────────────────────────────────────

export type KeywordRow = {
  id: number;
  name: string;
  count: number;
};

export function keywordsList(): Promise<KeywordRow[]> {
  return invoke<KeywordRow[]>("keywords_list", {});
}

export function keywordsForImage(imageId: number): Promise<KeywordRow[]> {
  return invoke<KeywordRow[]>("keywords_for_image", { imageId });
}

export function keywordAddToImage(
  imageId: number,
  name: string,
): Promise<KeywordRow> {
  return invoke<KeywordRow>("keyword_add_to_image", { imageId, name });
}

export function keywordAddToImages(
  imageIds: number[],
  name: string,
): Promise<KeywordRow> {
  return invoke<KeywordRow>("keyword_add_to_images", { imageIds, name });
}

export function keywordRemoveFromImage(
  imageId: number,
  keywordId: number,
): Promise<void> {
  return invoke<void>("keyword_remove_from_image", { imageId, keywordId });
}

export function keywordDelete(keywordId: number): Promise<void> {
  return invoke<void>("keyword_delete", { keywordId });
}

// ── Collections ──────────────────────────────────────────────────────────────

export type CollectionRow = {
  id: number;
  name: string;
  isSmart: boolean;
  /** Predicate JSON (serialized QueryParams) for smart collections; null for static. */
  query: string | null;
  count: number;
};

export function collectionsList(): Promise<CollectionRow[]> {
  return invoke<CollectionRow[]>("collections_list", {});
}

export function collectionsForImage(imageId: number): Promise<CollectionRow[]> {
  return invoke<CollectionRow[]>("collections_for_image", { imageId });
}

export function collectionCreate(
  name: string,
  isSmart: boolean,
  query: string | null,
): Promise<number> {
  return invoke<number>("collection_create", { name, isSmart, query });
}

export function collectionRename(id: number, name: string): Promise<void> {
  return invoke<void>("collection_rename", { id, name });
}

export function collectionDelete(id: number): Promise<void> {
  return invoke<void>("collection_delete", { id });
}

export function collectionAddImages(
  collectionId: number,
  imageIds: number[],
): Promise<number> {
  return invoke<number>("collection_add_images", { collectionId, imageIds });
}

export function collectionRemoveImages(
  collectionId: number,
  imageIds: number[],
): Promise<number> {
  return invoke<number>("collection_remove_images", { collectionId, imageIds });
}

/**
 * Extract the smart-collection predicate from params. Captures the persistent filter dimensions
 * only — NOT free-text `search` (transient, and not reset by clearedFilters, so it would leak when
 * toggling a smart collection off) nor `collectionId` (a smart collection defined by membership in
 * another collection would be circular). Every captured key is in FILTER_DIMENSIONS, so applying /
 * clearing a smart collection round-trips cleanly. Key order is fixed for stable === comparison.
 */
export function smartQueryFromParams(p: QueryParams): string {
  const pred: QueryParams = {};
  if (p.folderId != null) pred.folderId = p.folderId;
  if (p.minStars != null) pred.minStars = p.minStars;
  if (p.flag != null) pred.flag = p.flag;
  if (p.colorLabel != null) pred.colorLabel = p.colorLabel;
  if (p.keywordId != null) pred.keywordId = p.keywordId;
  if (p.importSessionId != null) pred.importSessionId = p.importSessionId;
  return JSON.stringify(pred);
}

/** Parse a smart collection's stored predicate JSON back into QueryParams (safe). */
export function parseSmartQuery(query: string | null): Partial<QueryParams> {
  if (!query) return {};
  try {
    return JSON.parse(query) as Partial<QueryParams>;
  } catch {
    return {};
  }
}

// ── Develop IPC ────────────────────────────────────────────────────────────

export type CurvePoint = { x: number; y: number };

/** Per-channel tone curves; empty array on a channel = identity (no-op). */
export type ToneCurve = {
  rgb: CurvePoint[];
  r: CurvePoint[];
  g: CurvePoint[];
  b: CurvePoint[];
};

export type ToneCurveChannel = keyof ToneCurve;

/** One hue band of the HSL/color mixer; h/s/l each -100..100. */
export type HslBand = { h: number; s: number; l: number };

/** Number of hue bands (must match Rust `HSL_BANDS`). */
export const HSL_BANDS = 8;

/** Local adjustment set a mask carries (deltas on top of the global develop). Mirrors Rust `LocalAdjust`. */
export type LocalAdjust = {
  exposure: number;
  temp: number;
  tint: number;
  contrast: number;
  saturation: number;
  highlights: number;
  shadows: number;
  blacks: number;
  whites: number;
};

export const DEFAULT_LOCAL_ADJUST: LocalAdjust = {
  exposure: 0,
  temp: 0,
  tint: 0,
  contrast: 0,
  saturation: 0,
  highlights: 0,
  shadows: 0,
  blacks: 0,
  whites: 0,
};

/** One brush stroke (bezier control points normalized to the longest edge). Mirrors Rust `BrushStroke`. */
export type BrushStroke = {
  points: [number, number][];
  size: number;
  hardness: number;
  flow: number;
  opacity: number;
  isErase: boolean;
};

/** A mask component's shape/source (serde-tagged enum, `type` discriminant). Mirrors Rust `ComponentKind`. */
export type ComponentKind =
  | { type: "linear"; p0: [number, number]; p1: [number, number] }
  | {
      type: "radial";
      center: [number, number];
      radius: [number, number];
      angle: number;
      feather: number;
    }
  | { type: "brush"; strokes: BrushStroke[] }
  | { type: "luminanceRange"; lo: number; hi: number; feather: number }
  | {
      type: "colorRange";
      hue: number;
      sat: number;
      tol: number;
      feather: number;
    }
  | { type: "ai"; model: string };

/** How a component combines with the running mask alpha. Mirrors Rust `MaskOp`. */
export type MaskOp = "add" | "subtract" | "intersect";

/** One component of a mask. Mirrors Rust `MaskComponent`. */
export type MaskComponent = {
  kind: ComponentKind;
  op: MaskOp;
  invert: boolean;
  /** Request guided-filter edge-aware refinement (brush/range only). */
  feather: boolean;
};

/** A local adjustment mask. Mirrors Rust `Mask`. */
export type Mask = {
  name: string;
  components: MaskComponent[];
  adjust: LocalAdjust;
  opacity: number;
  enabled: boolean;
};

/** Maximum masks per image (must match Rust `MASK_CAP`). */
export const MASK_CAP = 16;

/** Crop + straighten geometry. Mirrors Rust `Crop`. Center (cx,cy) + half-extents (hw,hh) in
 * normalized image coords; `angle` is the straighten correction in degrees. Full frame = identity. */
export type Crop = {
  cx: number;
  cy: number;
  hw: number;
  hh: number;
  angle: number;
};

export const DEFAULT_CROP: Crop = {
  cx: 0.5,
  cy: 0.5,
  hw: 0.5,
  hh: 0.5,
  angle: 0,
};

/** A grading-RGB color offset (per-channel). Mirrors Rust `[f32; 3]`. */
export type Rgb3 = [number, number, number];

/** Color-balance-RGB grading (4-way + scene-linear contrast/saturation). Mirrors Rust `CbRgb`.
 * `global` = offset (all tones), `shadows` = lift, `highlights` = gain, `midtones` = per-channel
 * power; each a grading-RGB vector ≈ ±0.5. `contrast`/`saturation` are -1..1. All 0 = no-op. */
export type CbRgb = {
  global: Rgb3;
  shadows: Rgb3;
  midtones: Rgb3;
  highlights: Rgb3;
  contrast: number;
  saturation: number;
};

export const DEFAULT_CB_RGB: CbRgb = {
  global: [0, 0, 0],
  shadows: [0, 0, 0],
  midtones: [0, 0, 0],
  highlights: [0, 0, 0],
  contrast: 0,
  saturation: 0,
};

export type DevelopParams = {
  exposure: number;
  temp: number;
  tint: number;
  contrast: number;
  saturation: number;
  highlights: number;
  shadows: number;
  blacks: number;
  whites: number;
  sharpen: number;
  nrLuma: number;
  nrColor: number;
  vignette: number;
  /** Scene-referred base tone operator strength, 0..100 (0 = flat, 100 = full ACR look). */
  toneAmount: number;
  toneCurve: ToneCurve;
  hsl: HslBand[];
  crop: Crop;
  masks: Mask[];
  cbRgb: CbRgb;
};

/** The numeric (scalar) develop params — everything except the structured fields. */
export type ScalarParamKey = Exclude<
  keyof DevelopParams,
  "toneCurve" | "hsl" | "crop" | "masks" | "cbRgb"
>;

export const EMPTY_TONE_CURVE: ToneCurve = { rgb: [], r: [], g: [], b: [] };

export const DEFAULT_PARAMS: DevelopParams = {
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
  toneAmount: 100,
  toneCurve: { rgb: [], r: [], g: [], b: [] },
  hsl: Array.from({ length: HSL_BANDS }, () => ({ h: 0, s: 0, l: 0 })),
  crop: { ...DEFAULT_CROP },
  masks: [],
  cbRgb: { ...DEFAULT_CB_RGB },
};

export function developGetEdit(imageId: number): Promise<DevelopParams> {
  return invoke<DevelopParams>("develop_get_edit", { imageId });
}

export function developSetEdit(
  imageId: number,
  params: DevelopParams,
  /** Slider interactions in this edit session — a deliberation weight for the behavioral log. */
  touchCount?: number,
  /** Force overwrite even an unreadable stored blob (explicit Reset only). */
  force?: boolean,
): Promise<void> {
  return invoke<void>("develop_set_edit", {
    imageId,
    params,
    touchCount,
    force,
  });
}

/** Per-channel 256-bin histogram from the rendered buffer. */
export type HistData = { r: number[]; g: number[]; b: number[] };

// Monotonic across the whole session (survives component remounts) so the backend can identify
// and skip superseded render requests.
let renderRequestSeq = 0;

export type ViewRect = { ox: number; oy: number; sx: number; sy: number };

/** Rendered frame pixel data, or null when the request was superseded. */
export type RenderedFrame = { data: Uint8ClampedArray; w: number; h: number };

/**
 * Render the develop viewport at display resolution.
 *
 * The backend returns raw bytes: [outW u32 LE][outH u32 LE][rgba8 outW*outH*4].
 * An empty ArrayBuffer means the request was superseded — returns null.
 *
 * @param view      Visible window in crop-local uv [0,1] (ox,oy = top-left, sx,sy = size)
 * @param outW/outH Canvas backing store size in device px (= visCssSize * clamped-DPR)
 * @param overlayMaskIndex  Selected mask index (or -1 = no overlay)
 */
export async function developRender(
  imageId: number,
  params: DevelopParams,
  view: ViewRect,
  outW: number,
  outH: number,
  overlayMaskIndex: number,
): Promise<RenderedFrame | null> {
  const requestId = ++renderRequestSeq;
  const buf = await invoke<ArrayBuffer>("develop_render", {
    imageId,
    params,
    view,
    outW,
    outH,
    overlayMaskIndex,
    requestId,
  });
  if (buf.byteLength < 8) return null; // superseded or error
  const header = new DataView(buf, 0, 8);
  const w = header.getUint32(0, true); // little-endian
  const h = header.getUint32(4, true);
  const pixels = new Uint8ClampedArray(buf, 8);
  return { data: pixels, w, h };
}

/**
 * Instant first paint: the camera's embedded preview JPEG (demosaic-free, no edits applied).
 * Returns an object URL backed by JPEG bytes. Caller must revoke when done.
 */
export async function developPreviewJpeg(imageId: number): Promise<string> {
  const buf = await invoke<ArrayBuffer>("develop_preview_jpeg", { imageId });
  return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
}

/**
 * Library loupe source: the unedited capture (camera embedded preview, near full sensor res).
 * `maxEdge === 0` returns native size (capped backend-side); positive downscales the long edge.
 * Returns an object URL backed by JPEG bytes. Caller must revoke when done.
 */
export async function loupeJpeg(
  imageId: number,
  maxEdge: number,
): Promise<string> {
  const buf = await invoke<ArrayBuffer>("loupe_jpeg", { imageId, maxEdge });
  return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
}

/** Pull the most recent render's histogram (reliable fallback for the fire-and-forget event). */
export function developGetHistogram(): Promise<HistData | null> {
  return invoke<HistData | null>("develop_get_histogram", {});
}

/** Real per-image histogram (from the cached thumbnail) for the Library metadata panel. */
export function imageHistogram(imageId: number): Promise<HistData | null> {
  return invoke<HistData | null>("image_histogram", { imageId });
}

export function exportImage(
  imageId: number,
  params: DevelopParams,
  format: "png" | "jpeg",
  dest: string,
): Promise<void> {
  return invoke<void>("export_image", { imageId, params, format, dest });
}

// ── AI scan analysis (object detection + caption) ────────────────────────────

/** The three detected-object buckets, in display order. */
export const DETECTION_CATEGORIES = ["People", "Animals", "Vehicles"] as const;
export type DetectionCategory = (typeof DETECTION_CATEGORIES)[number];

/** One detected object. `bbox` is normalized `[x0,y0,x1,y1]` in [0,1]. */
export type Detection = {
  label: string;
  category: string;
  confidence: number;
  bbox: [number, number, number, number];
};

export type ImageCaption = {
  caption: string;
  keywords: string[];
};

/** Detected-object category count (distinct images) for the LeftNav facet. */
export type FacetRow = {
  category: string;
  count: number;
};

export type AnalysisStatus = {
  total: number;
  analyzed: number;
  pending: number;
  modelsReady: boolean;
  running: boolean;
};

export type AnalysisRunStats = {
  analyzed: number;
  failed: number;
};

/** Total/analyzed/pending counts + models-ready/running flags. */
export function analysisStatus(): Promise<AnalysisStatus> {
  return invoke<AnalysisStatus>("analysis_status", {});
}

/** Download missing model files (first run). Emits `analysis:models` `{done,total}`. */
export function analysisModelsEnsure(): Promise<void> {
  return invoke<void>("analysis_models_ensure", {});
}

/** Run the background analysis pass. Emits `analysis:progress` `{done,total}` then `analysis:done`. */
export function analysisRun(force = false): Promise<AnalysisRunStats> {
  return invoke<AnalysisRunStats>("analysis_run", { force });
}

/** Request the running pass to stop after the current batch (keeps work already committed). */
export function analysisCancel(): Promise<void> {
  return invoke<void>("analysis_cancel", {});
}

/** Backfill per-image feature vectors (lighting/best-shot/dedup model inputs) for images missing
 *  them. Emits `features:progress` `{done,total}` then `features:done`. Resolves to count computed. */
export function featuresBackfill(): Promise<number> {
  return invoke<number>("features_backfill", {});
}

/** Write a `<raw>.json` sidecar (edits + rating + keywords) next to every present RAW. Migrates an
 *  existing catalog onto the durable on-disk format. Resolves to the count written. */
export function sidecarsWriteAll(): Promise<number> {
  return invoke<number>("sidecars_write_all", {});
}

/** Force-apply every present image's sidecar back into the catalog (recover edits/ratings/keywords
 *  after a catalog loss / across machines). Resolves to the count hydrated. */
export function sidecarsRebuild(): Promise<number> {
  return invoke<number>("sidecars_rebuild", {});
}

/** Per-category detected-image counts. */
export function analysisFacets(): Promise<FacetRow[]> {
  return invoke<FacetRow[]>("analysis_facets", {});
}

export function imageDetections(id: number): Promise<Detection[]> {
  return invoke<Detection[]>("image_detections", { id });
}

export function imageCaption(id: number): Promise<ImageCaption | null> {
  return invoke<ImageCaption | null>("image_caption", { id });
}

/** MobileCLIP presence-probe scores in [0,1] (advisory AI readout). `null` until the probe ran. */
export type Presence = {
  pPerson: number;
  pAnimal: number;
};

export function imagePresence(id: number): Promise<Presence | null> {
  return invoke<Presence | null>("image_presence", { id });
}

/** Manual ground-truth labels (tri-state: `null` = unlabeled). Doubles as detection eval data. */
export type UserLabels = {
  containsPerson: boolean | null;
  containsAnimal: boolean | null;
};

export function imageUserLabels(id: number): Promise<UserLabels> {
  return invoke<UserLabels>("image_user_labels", { id });
}

/** Set one label field (`"person"` | `"animal"`); `value = null` clears it. */
export function setImageUserLabel(
  id: number,
  field: "person" | "animal",
  value: boolean | null,
): Promise<void> {
  return invoke<void>("set_image_user_label", { id, field, value });
}

/** Set one label field on many images at once (multi-select labeling). */
export function setImageUserLabelMany(
  imageIds: number[],
  field: "person" | "animal",
  value: boolean | null,
  groupId?: string,
): Promise<void> {
  return invoke<void>("set_image_user_label_many", {
    imageIds,
    field,
    value,
    groupId: groupId ?? null,
  });
}

/** MegaDetector (animal) input resolution: 640 (faster) or 1280 (best recall). */
export function analysisDetectorSize(): Promise<number> {
  return invoke<number>("analysis_detector_size", {});
}

export function setAnalysisDetectorSize(size: number): Promise<void> {
  return invoke<void>("set_analysis_detector_size", { size });
}

// ── Faces / People ───────────────────────────────────────────────────────────

/** Face lifecycle status. */
export type FaceStatus = "unconfirmed" | "confirmed" | "rejected" | "ignored";

/** A person/cluster for the sidebar. `name` null = an unnamed "Suggested" cluster. The cover fields
 *  let the UI CSS-crop a face thumbnail from the person's best photo (see {@link faceCropStyle}). */
export type PersonRow = {
  id: number;
  name: string | null;
  hidden: boolean;
  faceCount: number;
  coverFaceId: number | null;
  coverImageHash: string | null;
  /** Normalized `[x1,y1,x2,y2]` of the cover face. */
  coverBbox: [number, number, number, number] | null;
};

/** One face of a person (person detail / Review grid). `bbox` normalized `[x1,y1,x2,y2]`. */
export type PersonFace = {
  id: number;
  imageId: number;
  imageHash: string;
  bbox: [number, number, number, number];
  status: FaceStatus;
  detScore: number;
  quality: number;
};

/** A face detected in one image (RightInfo chips). */
export type ImageFace = {
  id: number;
  personId: number | null;
  personName: string | null;
  bbox: [number, number, number, number];
  status: FaceStatus;
};

export type FacesStatus = {
  total: number;
  processed: number;
  pending: number;
  modelsReady: boolean;
  running: boolean;
  faces: number;
  people: number;
};

export type ClusterStats = {
  assigned: number;
  newPeople: number;
  deferred: number;
};
export type FacesRunStats = {
  images: number;
  faces: number;
  cluster: ClusterStats;
};

/** People status: counts + model/running state. */
export function facesStatus(): Promise<FacesStatus> {
  return invoke<FacesStatus>("faces_status", {});
}

/** Download the face models (~190 MB, first run). Emits `faces:models` `{done,total}`. */
export function facesModelsEnsure(): Promise<void> {
  return invoke<void>("faces_models_ensure", {});
}

/** Run "Find People" (detect → align → embed → cluster). Emits `faces:progress`/`faces:done`. */
export function facesRun(force = false): Promise<FacesRunStats> {
  return invoke<FacesRunStats>("faces_run", { force });
}

/** Request the running face pass to stop after the current batch. */
export function facesCancel(): Promise<void> {
  return invoke<void>("faces_cancel", {});
}

export function peopleList(includeHidden = false): Promise<PersonRow[]> {
  return invoke<PersonRow[]>("people_list", { includeHidden });
}

/** Faces of a person, optionally a single status (e.g. "unconfirmed" for Review). */
export function personFaces(
  personId: number,
  status?: FaceStatus,
): Promise<PersonFace[]> {
  return invoke<PersonFace[]>("person_faces", {
    personId,
    status: status ?? null,
  });
}

export function imageFaces(id: number): Promise<ImageFace[]> {
  return invoke<ImageFace[]>("image_faces", { id });
}

/** Set or clear (`null`) a person's name. */
export function personSetName(
  personId: number,
  name: string | null,
): Promise<void> {
  return invoke<void>("person_set_name", { personId, name });
}

export function personSetHidden(
  personId: number,
  hidden: boolean,
): Promise<void> {
  return invoke<void>("person_set_hidden", { personId, hidden });
}

export function personSetCover(
  personId: number,
  faceId: number,
): Promise<void> {
  return invoke<void>("person_set_cover", { personId, faceId });
}

/** Merge person `src` into `dst` (move all faces, delete `src`). Not reversible. */
export function personMerge(dst: number, src: number): Promise<void> {
  return invoke<void>("person_merge", { dst, src });
}

export function faceConfirm(faceId: number): Promise<void> {
  return invoke<void>("face_confirm", { faceId });
}

export function faceReject(faceId: number): Promise<void> {
  return invoke<void>("face_reject", { faceId });
}

/** Reassign a face to a person (confirmed), or `null` to unlink it. */
export function faceAssign(
  faceId: number,
  personId: number | null,
): Promise<void> {
  return invoke<void>("face_assign", { faceId, personId });
}

/** Delete ALL face + person data (privacy). Not reversible. */
export function facesDeleteAll(): Promise<void> {
  return invoke<void>("faces_delete_all", {});
}

/** Inline-style props that crop a face out of its image thumbnail (a CSS sprite crop), padded for a
 *  pleasant headshot. `bbox` is normalized `[x1,y1,x2,y2]`; the thumbnail is aspect-preserving and
 *  EXIF-oriented, matching the (also oriented) face coordinates. */
export function faceCropStyle(
  hash: string,
  bbox: [number, number, number, number],
  pad = 0.4,
): CSSProperties {
  const [x1, y1, x2, y2] = bbox;
  const bw = x2 - x1;
  const bh = y2 - y1;
  // Pad the box (clamped) so the crop isn't tight on the face.
  const px = bw * pad;
  const py = bh * pad;
  const cx1 = Math.max(0, x1 - px);
  const cy1 = Math.max(0, y1 - py);
  const cx2 = Math.min(1, x2 + px);
  const cy2 = Math.min(1, y2 + py);
  const cw = Math.max(1e-3, cx2 - cx1);
  const ch = Math.max(1e-3, cy2 - cy1);
  // Standard sprite math: scale the image up so the crop fills the element, then position it.
  const posX = cw < 1 ? (cx1 / (1 - cw)) * 100 : 0;
  const posY = ch < 1 ? (cy1 / (1 - ch)) * 100 : 0;
  return {
    backgroundImage: `url("${thumbUrl(hash)}")`,
    backgroundRepeat: "no-repeat",
    backgroundSize: `${100 / cw}% ${100 / ch}%`,
    backgroundPosition: `${posX}% ${posY}%`,
  };
}
