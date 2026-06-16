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
): Promise<ImportStats> {
  return invoke<ImportStats>("import_start", { source, mode, dest });
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
): Promise<number> {
  return invoke<number>("dedup_resolve", { keepId, trashIds });
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

export function thumbUrl(hash: string, size = 512): string {
  return `thumb://localhost/${hash}?size=${size}`;
}

// ── Cull IPC ───────────────────────────────────────────────────────────────

export function cullSetRating(imageId: number, stars: number): Promise<void> {
  return invoke<void>("cull_set_rating", { imageId, stars });
}

export function cullSetFlag(
  imageId: number,
  flag: "none" | "pick" | "reject",
): Promise<void> {
  return invoke<void>("cull_set_flag", { imageId, flag });
}

export function cullSetLabel(
  imageId: number,
  label: string | null,
): Promise<void> {
  return invoke<void>("cull_set_label", { imageId, label });
}

// Batch culling (apply one value to a whole selection).

export function cullSetRatingMany(
  imageIds: number[],
  stars: number,
): Promise<void> {
  return invoke<void>("cull_set_rating_many", { imageIds, stars });
}

export function cullSetFlagMany(
  imageIds: number[],
  flag: "none" | "pick" | "reject",
): Promise<void> {
  return invoke<void>("cull_set_flag_many", { imageIds, flag });
}

export function cullSetLabelMany(
  imageIds: number[],
  label: string | null,
): Promise<void> {
  return invoke<void>("cull_set_label_many", { imageIds, label });
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
  toneCurve: ToneCurve;
  hsl: HslBand[];
  masks: Mask[];
};

/** The numeric (scalar) develop params — everything except the structured curve/hsl fields. */
export type ScalarParamKey = Exclude<keyof DevelopParams, "toneCurve" | "hsl">;

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
  toneCurve: { rgb: [], r: [], g: [], b: [] },
  hsl: Array.from({ length: HSL_BANDS }, () => ({ h: 0, s: 0, l: 0 })),
  masks: [],
};

export function developGetEdit(imageId: number): Promise<DevelopParams> {
  return invoke<DevelopParams>("develop_get_edit", { imageId });
}

export function developSetEdit(
  imageId: number,
  params: DevelopParams,
): Promise<void> {
  return invoke<void>("develop_set_edit", { imageId, params });
}

/** Per-channel 256-bin histogram from the rendered buffer. */
export type HistData = { r: number[]; g: number[]; b: number[] };

// Monotonic across the whole session (survives component remounts) so the backend can identify
// and skip superseded render requests.
let renderRequestSeq = 0;

/**
 * Render the develop preview. Returns an object URL backed by JPEG bytes (caller must revoke), or
 * `null` if the backend skipped this request because a newer one superseded it.
 */
export async function developRender(
  imageId: number,
  params: DevelopParams,
): Promise<string | null> {
  const requestId = ++renderRequestSeq;
  const buf = await invoke<ArrayBuffer>("develop_render", {
    imageId,
    params,
    requestId,
  });
  if (buf.byteLength === 0) return null; // superseded — no-op
  return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
}

/**
 * Instant first paint: the camera's embedded preview JPEG (demosaic-free, no edits applied).
 * Returns an object URL backed by JPEG bytes. Caller must revoke when done.
 */
export async function developPreviewJpeg(imageId: number): Promise<string> {
  const buf = await invoke<ArrayBuffer>("develop_preview_jpeg", { imageId });
  return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
}

/** Pull the most recent render's histogram (reliable fallback for the fire-and-forget event). */
export function developGetHistogram(): Promise<HistData | null> {
  return invoke<HistData | null>("develop_get_histogram", {});
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
