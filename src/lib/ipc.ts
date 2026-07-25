import type { CSSProperties } from "react";
import { invoke as tauriInvoke } from "@tauri-apps/api/core";
import { log } from "./logger";

function summarizeArgs(args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    if (
      ["path", "source", "dest", "filename", "search"].some((s) =>
        key.toLowerCase().includes(s),
      )
    ) {
      out[key] = "[redacted]";
    } else if (Array.isArray(value)) {
      out[key] = { count: value.length };
    } else if (value && typeof value === "object") {
      out[key] = "[object]";
    } else {
      out[key] = value;
    }
  }
  return out;
}

function summarizeResult(value: unknown): Record<string, unknown> {
  if (Array.isArray(value)) return { resultCount: value.length };
  if (value instanceof Uint8Array || value instanceof ArrayBuffer)
    return { resultBytes: value.byteLength };
  if (value && typeof value === "object") return { resultType: "object" };
  return { resultType: typeof value };
}

async function invoke<T>(
  command: string,
  args: Record<string, unknown>,
): Promise<T> {
  if (command === "frontend_log") return tauriInvoke<T>(command, args);
  const start = performance.now();
  log.debug("ipc", "invoke start", { command, args: summarizeArgs(args) });
  try {
    const result = await tauriInvoke<T>(command, args);
    log.debug("ipc", "invoke success", {
      command,
      durationMs: Math.round(performance.now() - start),
      ...summarizeResult(result),
    });
    return result;
  } catch (err) {
    log.warn("ipc", "invoke failed", {
      command,
      durationMs: Math.round(performance.now() - start),
      ...log.errorSummary(err),
    });
    throw err;
  }
}

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
  /** Capture-year folder filter ("2026" | "Unknown"), matched against capture_date (UTC). */
  captureYear?: string | null;
  /** Capture-day folder filter ("2026-06-22" | "Unknown"), matched against capture_date (UTC). */
  captureDate?: string | null;
  /** Detected-object bucket filter: "People" | "Animals" | "Vehicles". */
  detectedCategory?: string | null;
  /** Restrict to images containing a (confirmed or suggested) face of this person. */
  personId?: number | null;
  /** Source format bucket filter: "raw" | "jpeg" | "png" | "heif" | "hdr". */
  format?: string | null;
  search?: string | null;
  sort?: SortKey;
  limit?: number;
  offset?: number;
  /** Keyset (seek) cursor: capture_date / imported_at of the last loaded row. null + null cursorId =
   *  first page; null value + set cursorId = a cursor inside the NULL capture_date block. */
  cursorValue?: number | null;
  /** Keyset cursor: id of the last loaded row (tie-break). Presence marks "has cursor". */
  cursorId?: number | null;
  /** Use keyset/seek pagination (time-based sorts) instead of LIMIT/OFFSET. */
  seek?: boolean;
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
  "captureYear",
  "captureDate",
  "detectedCategory",
  "personId",
  "format",
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
    captureYear: null,
    captureDate: null,
    detectedCategory: null,
    personId: null,
    format: null,
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
  /** When the image was catalogued (epoch seconds): keyset cursor for import-date sorts + a live
   *  sorted-merge comparator key. */
  importedAt: number;
  /** Source format bucket ("raw" | "jpeg" | "png" | "heif" | "hdr"); null for legacy rows
   *  predating the column. */
  format: string | null;
};

export type FolderRow = {
  id: number;
  path: string;
  count: number;
};

/** A capture-day node within a year of the Folders date tree. */
export type DateNode = {
  /** "YYYY-MM-DD" (UTC) or "Unknown". */
  date: string;
  count: number;
};

/** A capture-year node of the Folders date tree (Lightroom-style Year → Date). */
export type DateTreeYear = {
  /** "YYYY" (UTC) or "Unknown". */
  year: string;
  count: number;
  dates: DateNode[];
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

/** Year → Date capture-date tree for the left-nav Folders section. */
export function libraryDateTree(): Promise<DateTreeYear[]> {
  return invoke<DateTreeYear[]>("library_date_tree", {});
}

export function imageMeta(id: number): Promise<ImageRow | null> {
  return invoke<ImageRow | null>("image_meta", { id });
}

export type GpuStatus = {
  name: string;
  vendor: number;
  device: number;
  deviceType: string;
  backend: string;
  driver: string;
  driverInfo: string;
  devicePciBusId: string;
  subgroupMinSize: number;
  subgroupMaxSize: number;
  transientSavesMemory: boolean;
  maxTextureDim: number;
};

export function gpuStatus(): Promise<GpuStatus | null> {
  return invoke<GpuStatus | null>("gpu_status", {});
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

/** Content-hash dedup status of a source file (matches Rust `SourceStatus`). "pending" = not yet
 *  hash-checked (the listing default; resolved by `importDedup` in the background). */
export type SourceStatus =
  "pending" | "new" | "duplicateLibrary" | "duplicateBatch";

/** One source file in the fast import list — filesystem metadata only (matches Rust `SourceFile`).
 *  No thumbnail/hash up front; previews load lazily via `importThumb(path)`. */
export type SourceFile = {
  /** Absolute source path — the selection key passed to `importCommit`. */
  path: string;
  filename: string;
  sizeBytes: number;
  /** File modification time (epoch seconds) — fast stand-in for capture date in the list. */
  mtime: number;
  status: SourceStatus;
  /** Source format bucket ("raw" | "jpeg" | "png") — drives the by-type filter chips. */
  kind: string;
};

/** A resolved hash-dedup verdict for one path (matches Rust `DedupResult`). */
export type DedupResult = { path: string; status: SourceStatus };

/** "Apply During Import" options (matches Rust `ImportOptions`). All optional. */
export type ImportOptions = {
  rating?: number | null;
  flag?: "pick" | "reject" | null;
  keywords?: string[];
  collectionId?: number | null;
  newCollection?: string | null;
};

export type DupImage = {
  id: number;
  contentHash: string;
  path: string;
  filename: string;
  fileSize: number;
  captureDate: number | null;
  stars: number;
  iso: number | null;
  shutter: string | null;
  aperture: number | null;
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

/** Persist the library root (copy/move import destination). Creates the dir; existing photos are not
 *  moved — only future copy/move imports file there. */
export function setLibraryRoot(path: string): Promise<void> {
  return invoke<void>("set_library_root", { path });
}

/** Fast list of importable files under a source (filesystem metadata only — returns in ms, no
 *  hashing/decoding). Previews are loaded lazily per file via `importThumb`. */
export function importList(
  source: string,
  recursive = true,
): Promise<SourceFile[]> {
  return invoke<SourceFile[]>("import_list", { source, recursive });
}

/** Hash-verify dedup for listed source paths (size-prefiltered). Emits `import:dedup:progress`
 *  {done,total,results} as it resolves; resolves with the full verdict set. */
export function importDedup(paths: string[]): Promise<DedupResult[]> {
  return invoke<DedupResult[]>("import_dedup", { paths });
}

/** Lazily decode one source file's embedded preview → an object URL (caller must revoke it). */
export async function importThumb(
  path: string,
  maxEdge = 1024,
): Promise<string> {
  const buf = await invoke<ArrayBuffer>("import_thumb", { path, maxEdge });
  return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
}

/** Commit a staged import: copy/move/reference only `selected` source paths, then apply `options`.
 *  Emits `import:progress` (live rows) + `import:done`. */
export function importCommit(
  source: string,
  mode: ImportMode,
  dest: string,
  selected: string[],
  options: ImportOptions,
): Promise<ImportStats> {
  return invoke<ImportStats>("import_commit", {
    source,
    mode,
    dest,
    selected,
    options,
  });
}

/** Merge 2–9 bracketed RAW frames (tripod) into a scene-referred HDR EXR in the library.
 *  Long-running: emits `hdr:progress {done,total,stage}` then `hdr:done {image}` +
 *  `library:changed`; resolves with the new catalog row (format "hdr"). */
export function hdrMerge(imageIds: number[]): Promise<ImageRow> {
  return invoke<ImageRow>("hdr_merge", { imageIds });
}

/** Request the running HDR merge to stop (honored between frames). */
export function hdrCancel(): Promise<void> {
  return invoke<void>("hdr_cancel", {});
}

/** Export a merged-HDR image (`format === "hdr"`) as an uncompressed FLOAT LinearRaw DNG for
 *  Lightroom/ACR interop — the full >1.0 headroom Merge-to-HDR produced survives untouched. Export
 *  only: never re-imported into the catalog. */
export function hdrExportDng(imageId: number, dest: string): Promise<void> {
  return invoke<void>("hdr_export_dng", { imageId, dest });
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

/** Configured display-sharp preview longest edge (px), or 0 when unset (no default picked yet). */
export function previewEdge(): Promise<number> {
  return invoke<number>("preview_edge", {});
}

/** Persist the preview longest edge (px); backend clamps + re-renders previews at the new size. */
export function setPreviewEdge(edge: number): Promise<void> {
  return invoke<void>("set_preview_edge", { edge });
}

export type LogsStatus = {
  directory: string;
  sizeBytes: number;
  fileCount: number;
  level: "error" | "warn" | "info" | "debug" | "trace";
};

export function logsStatus(): Promise<LogsStatus> {
  return invoke<LogsStatus>("logs_status", {});
}

export function setLogsDirectory(path: string): Promise<LogsStatus> {
  return invoke<LogsStatus>("set_logs_directory", { path });
}

export function setLogLevel(level: LogsStatus["level"]): Promise<LogsStatus> {
  return invoke<LogsStatus>("set_log_level", { level });
}

export function logsExportZip(dest: string): Promise<number> {
  return invoke<number>("logs_export_zip", { dest });
}

export function logsDeleteAll(): Promise<LogsStatus> {
  return invoke<LogsStatus>("logs_delete_all", {});
}

/** Clamp bounds for the preview edge (mirror of the Rust `PREVIEW_EDGE_MIN/MAX`). */
export const PREVIEW_EDGE_MIN = 2560;
export const PREVIEW_EDGE_MAX = 4096;

/** The preview edge to use for `thumb://` preview URLs: the configured value, or a sensible default
 *  derived from the display resolution (longest screen edge × DPR, clamped). Persists the default the
 *  first time so the backend renders previews at the right size. Cached after first resolution. */
let previewEdgeCache: number | null = null;
export async function effectivePreviewEdge(): Promise<number> {
  if (previewEdgeCache != null) return previewEdgeCache;
  let edge = 0;
  try {
    edge = await previewEdge();
  } catch {
    edge = 0;
  }
  if (edge <= 0) {
    const dpr =
      typeof window !== "undefined"
        ? Math.min(window.devicePixelRatio || 1, 2)
        : 1;
    const longest =
      typeof window !== "undefined"
        ? Math.max(window.screen?.width ?? 0, window.screen?.height ?? 0) * dpr
        : 0;
    edge = Math.round(
      Math.min(
        PREVIEW_EDGE_MAX,
        Math.max(PREVIEW_EDGE_MIN, longest || PREVIEW_EDGE_MAX),
      ),
    );
    // Persist so the backend renders previews at this size (fire-and-forget).
    void setPreviewEdge(edge).catch(() => {});
  }
  previewEdgeCache = edge;
  return edge;
}

/** Change the preview edge from Settings: clamp, update the in-session cache (so new `thumb://`
 *  preview URLs target the new size), and persist (backend clamps + re-renders previews). Returns
 *  the clamped value applied. */
export async function updatePreviewEdge(edge: number): Promise<number> {
  const clamped = Math.round(
    Math.min(PREVIEW_EDGE_MAX, Math.max(PREVIEW_EDGE_MIN, edge)),
  );
  previewEdgeCache = clamped;
  await setPreviewEdge(clamped);
  return clamped;
}

// ── Utilities ──────────────────────────────────────────────────────────────

// Tauri v2 serves custom protocols as `scheme://localhost/…` on macOS/Linux but only as
// `http://scheme.localhost/…` on Windows (WebView2 cannot navigate the bare scheme). Mirror
// Tauri's own convertFileSrc switch. We can't call convertFileSrc directly: it
// encodeURIComponent-encodes its whole argument, which would mangle our query string.
const THUMB_BASE =
  typeof navigator !== "undefined" && navigator.userAgent.includes("Windows")
    ? "http://thumb.localhost"
    : "thumb://localhost";

export function thumbUrl(
  hash: string,
  size = 512,
  editedAt?: number | null,
  /** Cache-bust token (from `thumbVersions`): changes when the backend renders a fresh
   *  canonical/edited thumbnail, forcing the immutable-cached `<img>` to refetch. */
  token?: number | null,
  /** When > 0, request the display-sharp PREVIEW tier at this longest edge (loupe / develop
   *  first-paint) instead of the small thumb tier (grid / filmstrip). */
  previewEdge?: number | null,
): string {
  const base = `${THUMB_BASE}/${hash}?size=${size}`;
  // `edit=<version>` makes the protocol serve the edited render and changes the URL on each edit.
  // `pv=1&edge=<n>` requests the larger preview tier. `&t=<token>` busts the cache when a fresh
  // render lands for an UNEDITED image (placeholder → canonical swap), where `editedAt` doesn't change.
  const edited = editedAt != null ? `${base}&edit=${editedAt}` : base;
  const previewed =
    previewEdge != null && previewEdge > 0
      ? `${edited}&pv=1&edge=${previewEdge}`
      : edited;
  const url = token != null ? `${previewed}&t=${token}` : previewed;
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

/** Promote images to the front of the canonical-thumbnail backfill queue (visible range / the image
 *  opening in Develop) so they render before the bulk backfill. Fire-and-forget. */
export function thumbPrioritize(imageIds: number[]): Promise<void> {
  return invoke<void>("thumb_prioritize", { imageIds });
}

/** Tell the backend whether a Develop session is open, so the background thumbnail worker yields the
 *  GPU to interactive renders while editing. */
export function developSession(active: boolean): Promise<void> {
  return invoke<void>("develop_session", { active });
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
  | { type: "ai"; model: string; points: AiPoint[]; hash: string };

/** One prompt point for an AI (SAM) mask, normalized [0,1]. `positive=false` subtracts. Mirrors Rust `AiPoint`. */
export type AiPoint = { x: number; y: number; positive: boolean };

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
  /** Whole-image 90° rotation in clockwise quarter-turns (0..3). Applied as the outermost
   *  transform; cx/cy/hw/hh/angle are defined in the rotated (displayed) frame. */
  rot90: number;
};

export const DEFAULT_CROP: Crop = {
  cx: 0.5,
  cy: 0.5,
  hw: 0.5,
  hh: 0.5,
  angle: 0,
  rot90: 0,
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

/** Channel mixer (Photoshop/GIMP-style 3×3 remix on display sRGB). Mirrors Rust `ChannelMix`. Each
 * output channel is a weighted sum of the source R/G/B (1.0 = 100%). Identity = no-op; a pure swap
 * (e.g. green↔blue) is a permutation of the identity rows. */
export type ChannelMix = {
  red: Rgb3;
  green: Rgb3;
  blue: Rgb3;
};

export const DEFAULT_CHANNEL_MIX: ChannelMix = {
  red: [1, 0, 0],
  green: [0, 1, 0],
  blue: [0, 0, 1],
};

/** Border / frame around the image (solid color + optional pad-to-aspect). Mirrors Rust `Border`.
 * `size` = thickness as % of the long edge; `color` = sRGB 0..1; `aspectW/aspectH` = target output
 * ratio (0,0 = keep the photo's aspect). No-op at the default. */
export type Border = {
  size: number;
  color: Rgb3;
  aspectW: number;
  aspectH: number;
};

export const DEFAULT_BORDER: Border = {
  size: 0,
  color: [1, 1, 1],
  aspectW: 0,
  aspectH: 0,
};

/** True when the border would change the output (a margin, or a target aspect). */
export function borderIsActive(b: Border): boolean {
  return b.size > 0 || (b.aspectW > 0 && b.aspectH > 0);
}

/** AI raw-domain denoise settings (mirror of Rust `Denoise`). */
export type Denoise = {
  /** Whether denoise is applied to this image. */
  enabled: boolean;
  /** Blend amount, 0..100 of the denoised result over the original. */
  amount: number;
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
  /** Lens distortion primary radial coefficient (k1), -100..100. 0 = off. */
  distK1: number;
  /** Lens distortion secondary radial coefficient (k2), -100..100. 0 = off. */
  distK2: number;
  /** Lateral chromatic-aberration correction, red/cyan radial scale, -100..100. 0 = off. */
  caRed: number;
  /** Lateral chromatic-aberration correction, blue/yellow radial scale, -100..100. 0 = off. */
  caBlue: number;
  /** Clarity (mid-tone local contrast), -100..100. 0 = off. */
  clarity: number;
  /** Texture (fine local contrast), -100..100. 0 = off. */
  texture: number;
  /** Dehaze (coarse local contrast + black-point pull), -100..100. 0 = off. */
  dehaze: number;
  /** Scene-referred base tone operator strength, 0..100 (0 = flat, 100 = full ACR look). */
  toneAmount: number;
  toneCurve: ToneCurve;
  hsl: HslBand[];
  crop: Crop;
  masks: Mask[];
  cbRgb: CbRgb;
  channelMix: ChannelMix;
  border: Border;
  denoise: Denoise;
};

/** The numeric (scalar) develop params — everything except the structured fields. */
export type ScalarParamKey = Exclude<
  keyof DevelopParams,
  | "toneCurve"
  | "hsl"
  | "crop"
  | "masks"
  | "cbRgb"
  | "channelMix"
  | "border"
  | "denoise"
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
  distK1: 0,
  distK2: 0,
  caRed: 0,
  caBlue: 0,
  clarity: 0,
  texture: 0,
  dehaze: 0,
  toneAmount: 100,
  toneCurve: { rgb: [], r: [], g: [], b: [] },
  hsl: Array.from({ length: HSL_BANDS }, () => ({ h: 0, s: 0, l: 0 })),
  crop: { ...DEFAULT_CROP },
  masks: [],
  cbRgb: { ...DEFAULT_CB_RGB },
  channelMix: {
    red: [...DEFAULT_CHANNEL_MIX.red],
    green: [...DEFAULT_CHANNEL_MIX.green],
    blue: [...DEFAULT_CHANNEL_MIX.blue],
  },
  border: { ...DEFAULT_BORDER, color: [...DEFAULT_BORDER.color] },
  denoise: { enabled: false, amount: 50 },
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
export type RenderedFrame = {
  data: Uint8ClampedArray;
  w: number;
  h: number;
  previewSource?: boolean;
};

/**
 * Render the develop viewport at display resolution.
 *
 * The backend returns raw bytes: [outW u32 LE][outH u32 LE][flags?][rgba8 outW*outH*4].
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
  cropPreview = false,
): Promise<RenderedFrame | null> {
  const requestId = ++renderRequestSeq;
  const buf = await invoke<ArrayBuffer>("develop_render", {
    imageId,
    params,
    view,
    outW,
    outH,
    overlayMaskIndex,
    cropPreview,
    requestId,
  });
  if (buf.byteLength === 0) return null; // superseded
  if (buf.byteLength < 9) {
    log.error("develop", "render response too short", {
      byteLength: buf.byteLength,
    });
    return null;
  }
  const header = new DataView(buf, 0, 8);
  const w = header.getUint32(0, true); // little-endian
  const h = header.getUint32(4, true);
  // Strict framing: [w u32][h u32][flags u8][rgba w*h*4]. Never build ImageData from a
  // misaligned view — a mismatch means a backend framing bug, not a renderable frame.
  if (buf.byteLength !== 9 + w * h * 4) {
    log.error("develop", "render response size mismatch", {
      byteLength: buf.byteLength,
      w,
      h,
    });
    return null;
  }
  return {
    data: new Uint8ClampedArray(buf, 9),
    w,
    h,
    previewSource: new Uint8Array(buf, 8, 1)[0] !== 0,
  };
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
 * Predictively decode the half-res previews of `imageIds` (next/prev neighbors of the current
 * selection) into the backend's CPU cache, so stepping through a collection is instant.
 * Fire-and-forget; a newer call supersedes the previous set.
 */
export function developPrefetch(imageIds: number[]): Promise<void> {
  return invoke<void>("develop_prefetch", { imageIds });
}

/** Pull the most recent render's histogram (reliable fallback for the fire-and-forget event). */
export function developGetHistogram(): Promise<HistData | null> {
  return invoke<HistData | null>("develop_get_histogram", {});
}

/**
 * Compute the WHOLE-crop histogram for `params` (a small dedicated full-frame render, NOT the
 * viewport buffer — so it stays correct while zoomed). Stores it for pull + emits `develop:histogram`.
 * Call on param / before-after change (debounced), never on pan/zoom.
 */
export function developHistogram(
  imageId: number,
  params: DevelopParams,
): Promise<void> {
  return invoke<void>("develop_histogram", { imageId, params });
}

/** Real per-image histogram (from the cached thumbnail) for the Library metadata panel. */
export function imageHistogram(imageId: number): Promise<HistData | null> {
  return invoke<HistData | null>("image_histogram", { imageId });
}

/** One source frame that fed a merged image (HDR bracket or panorama stitch), for the metadata
 *  panel's "Source frames" section. Mirrors Rust `SourceRow`. */
export type SourceRow = {
  imageId: number;
  filename: string;
  /** Capture/bracket order (0-based), as recorded at merge time. */
  position: number;
  /** EV offset from the reference (metered) frame — HDR only; `null` for panorama sources. */
  relativeEv: number | null;
  /** `false` when the source's file is missing/relinked-away (the catalog link survives). */
  present: boolean;
};

/** The source frames behind one merged image, and which merge kind produced it. Mirrors Rust
 *  `MergeSources`. */
export type MergeSources = {
  kind: "hdr" | "panorama";
  /** Ordered by `position`. */
  sources: SourceRow[];
};

/** Source frames behind `imageId` (an HDR bracket or a panorama stitch), for the metadata panel's
 *  "Source frames" section. `null` when `imageId` is an ordinary, un-merged image. */
export function imageSources(imageId: number): Promise<MergeSources | null> {
  return invoke<MergeSources | null>("image_sources", { imageId });
}

export function exportImage(
  imageId: number,
  params: DevelopParams,
  format: "png" | "jpeg",
  dest: string,
  quality?: number,
): Promise<void> {
  return invoke<void>("export_image", { imageId, params, format, dest, quality });
}

// ── Presets + copy/paste settings ────────────────────────────────────────────

/** A preset row for the list panel (no params blob). Mirrors Rust `PresetSummary`. */
export type PresetSummary = {
  id: number;
  name: string;
  groupName: string;
  builtin: boolean;
  isFavorite: boolean;
  /** Touched top-level DevelopParams fields the preset sets. */
  fieldKeys: string[];
  sortOrder: number;
};

/** A full preset including its sparse params JSON string. Mirrors Rust `PresetFull`. */
export type PresetFull = PresetSummary & {
  params: string;
  processVersion: number;
};

/** One reported setting from an import (key + optional note). Mirrors Rust `ReportItem`. */
export type ReportItem = { key: string; note: string };

/** Honest conversion report for an imported preset. Mirrors Rust `ImportReport`. */
export type ImportReport = {
  sourceFormat: string;
  sourceProcessVersion: string | null;
  mapped: ReportItem[];
  approximated: ReportItem[];
  dropped: ReportItem[];
};

export type PresetImportResult = { presetId: number; report: ImportReport };

export function presetsList(): Promise<PresetSummary[]> {
  return invoke<PresetSummary[]>("presets_list", {});
}

export function presetsGet(presetId: number): Promise<PresetFull | null> {
  return invoke<PresetFull | null>("presets_get", { presetId });
}

/** Save the current edit (restricted to `fieldKeys`) as a new user preset; returns the new id. */
export function presetsSave(
  name: string,
  groupName: string | undefined,
  fieldKeys: string[],
  isFavorite: boolean,
  params: DevelopParams,
): Promise<number> {
  return invoke<number>("presets_save", {
    name,
    groupName,
    fieldKeys,
    isFavorite,
    params,
  });
}

export function presetsUpdate(
  presetId: number,
  patch: {
    name?: string;
    groupName?: string;
    isFavorite?: boolean;
    sortOrder?: number;
  },
): Promise<void> {
  return invoke<void>("presets_update", { presetId, ...patch });
}

export function presetsDelete(presetId: number): Promise<void> {
  return invoke<void>("presets_delete", { presetId });
}

export function presetsDuplicate(presetId: number): Promise<number> {
  return invoke<number>("presets_duplicate", { presetId });
}

/** Apply a preset onto an image's edit, blended by `amount` (0..1). Returns merged params (unsaved). */
export function presetsApply(
  imageId: number,
  presetId: number,
  amount: number,
  replaceAll: boolean,
): Promise<DevelopParams> {
  return invoke<DevelopParams>("presets_apply", {
    imageId,
    presetId,
    amount,
    replaceAll,
  });
}

export function presetsExport(
  presetId: number,
  destPath: string,
): Promise<void> {
  return invoke<void>("presets_export", { presetId, destPath });
}

export function presetsImportFile(
  srcPath: string,
): Promise<PresetImportResult> {
  return invoke<PresetImportResult>("presets_import_file", { srcPath });
}

/** Apply a copied source edit (subset to `fieldKeys`) onto an image. Returns merged params (unsaved). */
export function developApplySettings(
  imageId: number,
  params: DevelopParams,
  fieldKeys: string[],
  amount: number,
  replaceAll: boolean,
): Promise<DevelopParams> {
  return invoke<DevelopParams>("develop_apply_settings", {
    imageId,
    params,
    fieldKeys,
    amount,
    replaceAll,
  });
}

// ── Develop snapshots (persistent history) ───────────────────────────────────

export type SnapshotSummary = { id: number; name: string; createdAt: number };

export function snapshotsList(imageId: number): Promise<SnapshotSummary[]> {
  return invoke<SnapshotSummary[]>("snapshots_list", { imageId });
}

export function snapshotCreate(
  imageId: number,
  name: string,
  params: DevelopParams,
): Promise<number> {
  return invoke<number>("snapshot_create", { imageId, name, params });
}

/** Restore a snapshot's params (validated to the current schema). Caller commits to make it undoable. */
export function snapshotRestore(snapshotId: number): Promise<DevelopParams> {
  return invoke<DevelopParams>("snapshot_restore", { snapshotId });
}

export function snapshotRename(
  snapshotId: number,
  name: string,
): Promise<void> {
  return invoke<void>("snapshot_rename", { snapshotId, name });
}

export function snapshotDelete(snapshotId: number): Promise<void> {
  return invoke<void>("snapshot_delete", { snapshotId });
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
  /** Configured AI accelerator: "CoreML" | "DirectML" | "CPU" | "Unavailable". */
  accelerator: string;
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

// ── AI denoise ───────────────────────────────────────────────────────────────

export type DenoiseStatus = {
  running: boolean;
  /** false on the Intel-macOS build (no inference stack). */
  available: boolean;
  accelerator: string;
};

export function denoiseStatus(): Promise<DenoiseStatus> {
  return invoke<DenoiseStatus>("denoise_status", {});
}

/** Apply raw-domain denoise to `imageId` at `amount` (0..100). Swaps the denoised source into the
 *  render cache. Re-blends cached buffers on an amount change (no re-inference). Emits
 *  `denoise:progress` `{phase}` then `denoise:done` `{imageId}`; resolves when done. */
export function denoiseApply(imageId: number, amount: number): Promise<void> {
  return invoke<void>("denoise_apply", { imageId, amount });
}

/** Turn denoise off for `imageId` (drop cached buffers + evict the denoised render caches). */
export function denoiseClear(imageId: number): Promise<void> {
  return invoke<void>("denoise_clear", { imageId });
}

/** Request the running denoise compute to stop. */
export function denoiseCancel(): Promise<void> {
  return invoke<void>("denoise_cancel", {});
}

// ── AI model management ──────────────────────────────────────────────────────

/** Capability id shared by the manager IPC + `<id>:models` progress events. */
export type ModelGroupId = "analysis" | "faces" | "mask_ai";

/** One downloadable model file within a capability (or SAM tier). Mirrors Rust `ModelFileInfo`. */
export type ModelFileInfo = {
  rel: string;
  present: boolean;
  sizeBytes: number;
};

/** One SAM quality tier (AI Masking only). Mirrors Rust `ModelTierInfo`. */
export type ModelTierInfo = {
  tier: MaskAiTier;
  label: string;
  installed: boolean;
  sizeBytes: number;
  files: ModelFileInfo[];
};

/** One AI capability for the model manager. Mirrors Rust `model_mgmt::ModelGroup`. */
export type ModelGroup = {
  id: ModelGroupId;
  name: string;
  description: string;
  /** False on the Intel-macOS build (AI compiled out). */
  available: boolean;
  /** Ready to use (active tier for AI Masking). */
  installed: boolean;
  /** Installed on-disk bytes, else estimated download total. */
  sizeBytes: number;
  approxTotalBytes: number;
  license: string | null;
  files: ModelFileInfo[];
  /** Per-tier detail — AI Masking only; empty otherwise. */
  tiers: ModelTierInfo[];
  activeTier: MaskAiTier | null;
  accelerator: string;
};

/** Enriched `<id>:models` download-progress event payload: byte-level fields + legacy file counts. */
export type ModelDownloadEvent = {
  /** Legacy file index (1-based). */
  done: number;
  /** Legacy file count. */
  total: number;
  fileName: string;
  phase: "downloading" | "done";
  bytesDone: number;
  bytesTotal: number;
  fileBytesDone: number;
  fileBytesTotal: number;
};

/** Overview of all AI model capabilities: install state, sizes, per-file/per-tier detail. */
export function modelsOverview(): Promise<ModelGroup[]> {
  return invoke<ModelGroup[]>("models_overview", {});
}

/** Request an in-flight download for `group` to stop (discards the partial file). */
export function modelsCancel(group: ModelGroupId): Promise<void> {
  return invoke<void>("models_cancel", { group });
}

/** Delete a capability's model files. For `"mask_ai"`, `tier` picks the SAM tier (default active). */
export function modelsRemove(
  group: ModelGroupId,
  tier?: MaskAiTier,
): Promise<void> {
  return invoke<void>("models_remove", { group, tier: tier ?? null });
}

/** Result of an AI (SAM) mask prompt. Mirrors Rust `segment::PromptResult`. */
export type MaskAiResult = {
  /** Component key to store in the mask's `ai` component `hash`. */
  hash: string;
  width: number;
  height: number;
  iou: number;
};

/** Whether the AI-masking (SAM) models are downloaded and ready. */
export function maskAiReady(): Promise<boolean> {
  return invoke<boolean>("mask_ai_ready", {});
}

/** Download the AI-masking model weights (first use). Emits `mask_ai:models` `{done,total}`. */
export function maskAiModelsEnsure(): Promise<void> {
  return invoke<void>("mask_ai_models_ensure", {});
}

/** Warm the SAM embedding for an image (background encode) so the first click is instant. */
export function maskAiEncode(imageId: number): Promise<void> {
  return invoke<void>("mask_ai_encode", { imageId });
}

/** Segment an object on `imageId` from prompt `points` (normalized [0,1]). Returns the component
 *  key + mask dims + IoU; the caller writes `hash`/`points` into the mask's `ai` component. */
export function maskAiPrompt(
  imageId: number,
  points: AiPoint[],
): Promise<MaskAiResult> {
  return invoke<MaskAiResult>("mask_ai_prompt", { imageId, points });
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

/** AI-masking (SAM) quality tier. Only "realtime" is functional today. */
export type MaskAiTier = "realtime" | "balanced" | "max";

/** Configured AI-masking tier. */
export function maskAiTierGet(): Promise<MaskAiTier> {
  return invoke<MaskAiTier>("mask_ai_tier_get", {});
}

/** Persist the AI-masking tier (clears the SAM embedding so the next prompt re-encodes). */
export function maskAiTierSet(tier: MaskAiTier): Promise<void> {
  return invoke<void>("mask_ai_tier_set", { tier });
}

/** Whether the face stage runs as part of the unified AI scan (default on; needs face models). */
export function faceStageEnabled(): Promise<boolean> {
  return invoke<boolean>("face_stage_enabled", {});
}

export function setFaceStageEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_face_stage_enabled", { enabled });
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

// ── Panorama merge ───────────────────────────────────────────────────────────

/** Stitch projection surface. "auto" lets the backend pick from the source images' overlap/FOV. */
export type PanoramaProjection =
  | "auto"
  | "spherical"
  | "cylindrical"
  | "perspective";

/** Shared option set for `panoramaPreview`/`panoramaMerge` (mirrors the fixed Rust IPC contract —
 *  `panorama_preview`/`panorama_merge` take these fields; Tauri auto-converts the camelCase JS
 *  keys below to the Rust commands' snake_case params). */
export type PanoramaOptions = {
  imageIds: number[];
  projection: PanoramaProjection;
  /** 0..100; the backend clamps. Blends the seam boundary to hide parallax/exposure mismatches. */
  boundaryWarp: number;
  autoCrop: boolean;
  /** Originating panorama-detection group id, when this merge was handed off from
   *  `usePanoDetect().openMerge` — echoed back verbatim on the `panorama:done` event so the caller
   *  can mark the group merged without an ambient store field. `panoramaPreview` ignores it. */
  detectGroupId?: number | null;
};

/** Fast low-res preview of a panorama merge for the given options. Returns an object URL backed by
 *  JPEG bytes (caller must revoke). The command doesn't exist on the backend yet — an invoke failure
 *  (e.g. "command not found") should be handled by the caller via `isMergeEngineUnavailable`. */
export async function panoramaPreview(opts: PanoramaOptions): Promise<string> {
  const buf = await invoke<ArrayBuffer>("panorama_preview", {
    imageIds: opts.imageIds,
    projection: opts.projection,
    boundaryWarp: opts.boundaryWarp,
    autoCrop: opts.autoCrop,
  });
  return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
}

/** Run the full panorama merge for the given options. Resolves with the new image's id only once the
 *  whole merge finishes (register → bundle-adjust → warp → blend → crop → rectangle → encode);
 *  progress arrives via the `panorama:progress`/`panorama:done`/`panorama:error` events in the
 *  meantime (see `lib/usePanorama.ts`). */
export function panoramaMerge(opts: PanoramaOptions): Promise<number> {
  return invoke<number>("panorama_merge", {
    imageIds: opts.imageIds,
    projection: opts.projection,
    boundaryWarp: opts.boundaryWarp,
    autoCrop: opts.autoCrop,
    detectGroupId: opts.detectGroupId ?? null,
  });
}

/** Request the running panorama merge to stop. */
export function panoramaCancel(): Promise<void> {
  return invoke<void>("panorama_cancel", {});
}

/** Whether a panorama merge job is running in the backend right now (mirrors Rust `PanoStatus`).
 *  A merge outlives the renderer, so this is how the UI re-attaches its progress pill after a
 *  reload — the `panorama:*` events only tell you about transitions you were listening for. */
export type PanoramaStatus = { running: boolean };

export function panoramaStatus(): Promise<PanoramaStatus> {
  return invoke<PanoramaStatus>("panorama_status", {});
}

/** Drop the merge dialog's cached preview frames (called when the modal closes). */
export function panoramaPreviewRelease(): Promise<void> {
  return invoke<void>("panorama_preview_release", {});
}

// ── Panorama detection ───────────────────────────────────────────────────────

/** One source image within a detected panorama group, ordered by capture time (`position`). */
export type PanoMemberRow = {
  imageId: number;
  contentHash: string;
  filename: string;
  captureDate: number | null;
  format: string | null;
  position: number;
  /** False when the source image's file is missing (soft-deleted via reconcile — the group's link
   *  survives). `PanoSuggestions` dims this member and disables the merge handoff until it returns. */
  present: boolean;
};

/** A detected group of images the backend believes can be stitched into one panorama. */
export type PanoGroupRow = {
  id: number;
  /** Brown-Lowe confidence — the min necessary link on the group's max-confidence spanning tree. */
  confidence: number;
  status: "suggested" | "dismissed" | "merged";
  detectedAt: number;
  /** Set once the group has been merged (see `panoDetectMarkMerged`). */
  mergedImageId: number | null;
  /** True only when every member is a RAW source — the merge flow requires RAW inputs. */
  allRaw: boolean;
  members: PanoMemberRow[];
};

export type PanoDetectStatus = {
  running: boolean;
  /** Count of groups awaiting review (`status === "suggested"`). */
  suggested: number;
};

/** Kick off (or resume) the whole-library panorama-group scan in the background. Resolves with the
 *  number of suggested groups found once the pass completes; progress arrives via the
 *  `pano_detect:progress`/`pano_detect:done`/`pano_detect:error` events in the meantime (see
 *  `lib/usePanoDetect.ts`). `force` bypasses the per-image scan markers and rescans everything. */
export function panoDetectRun(force = false): Promise<number> {
  return invoke<number>("pano_detect_run", { force });
}

/** Request the running panorama-detection scan to stop. */
export function panoDetectCancel(): Promise<void> {
  return invoke<void>("pano_detect_cancel", {});
}

/** Current scan state + count of groups awaiting review. */
export function panoDetectStatus(): Promise<PanoDetectStatus> {
  return invoke<PanoDetectStatus>("pano_detect_status", {});
}

/** Detected panorama groups, most-recently-detected first. `includeDismissed` also returns groups
 *  the user has dismissed (still excludes nothing else — merged groups are always included). */
export function panoDetectGroups(includeDismissed = false): Promise<PanoGroupRow[]> {
  return invoke<PanoGroupRow[]>("pano_detect_groups", { includeDismissed });
}

/** Dismiss (or restore) a suggested panorama group without merging it. */
export function panoDetectDismiss(
  groupId: number,
  dismissed: boolean,
): Promise<void> {
  return invoke<void>("pano_detect_dismiss", { groupId, dismissed });
}

/** Mark a group merged once its handoff into the panorama merge flow lands a new image.
 *  `usePanoDetect`'s own `panorama:done` listener calls this and then refreshes the shared store —
 *  the refresh lives there, not here, so this module stays pure transport. */
export function panoDetectMarkMerged(
  groupId: number,
  mergedImageId: number,
): Promise<void> {
  return invoke<void>("pano_detect_mark_merged", { groupId, mergedImageId });
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
