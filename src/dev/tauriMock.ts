// Dev-only mock Tauri backend — lets the frontend run (and be Playwright-tested) in a plain
// Chromium browser, where there is no Tauri runtime and `invoke` would otherwise throw.
//
// Activated from main.tsx ONLY when `import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)`,
// so it is tree-shaken from production builds and stays inert inside the real `tauri dev` shell
// (which injects `__TAURI_INTERNALS__` before our code runs).
//
// What it provides:
//   • mockIPC handlers for the full ipc.ts command surface (returns fixture data)
//   • a fixture library of ImageRows + working filter/sort/paging + persistent cull edits
//   • thumb:// interception (rewrites <img> srcs to generated SVG placeholders)
//   • canvas-generated JPEG bytes for develop/loupe renders (responds to exposure/temp/tint)
//   • mocked windows + events (shouldMockEvents) so listen()/getCurrentWindow() don't hang
//
// See .claude/skills/darkroom-ui-testing for how this is driven via playwright-cli.

import {
  mockIPC,
  mockWindows,
  mockConvertFileSrc,
} from "@tauri-apps/api/mocks";
import type { InvokeArgs } from "@tauri-apps/api/core";
import {
  DEFAULT_PARAMS,
  LABEL_NONE,
  type ImageRow,
  type QueryParams,
} from "../lib/ipc";

const LIB_ROOT = "/Users/you/Pictures/Darkroom";
const FIXTURE_COUNT = 48;

// ── Fixture dataset ──────────────────────────────────────────────────────────

const FLAGS: ImageRow["flag"][] = ["none", "none", "pick", "none", "reject"];

function makeRows(n: number): ImageRow[] {
  const base = Math.floor(Date.UTC(2026, 4, 1) / 1000); // capture dates in epoch seconds
  return Array.from({ length: n }, (_, i): ImageRow => {
    const num = String(i + 1).padStart(4, "0");
    return {
      id: i + 1,
      contentHash: `mockhash${num}`,
      path: `${LIB_ROOT}/2026/2026-05-01/IMG_${num}.CR3`,
      filename: `IMG_${num}.CR3`,
      captureDate: base - i * 3600,
      cameraMake: "Canon",
      cameraModel: "Canon EOS R7",
      lens: "RF24-105mm F4 L IS USM",
      iso: 100 * (1 + (i % 6)),
      shutter: `1/${[125, 250, 500, 1000][i % 4]}`,
      aperture: [2.8, 4, 5.6, 8][i % 4],
      focalLength: [24, 35, 50, 85, 105][i % 5],
      width: 6960,
      height: 4640,
      orientation: 1,
      stars: i % 6,
      flag: FLAGS[i % FLAGS.length],
      colorLabel: null,
      editedAt: null,
    };
  });
}

const rows = makeRows(FIXTURE_COUNT);

// ── Query / filter / sort ────────────────────────────────────────────────────

function getParams(p: Record<string, unknown>): QueryParams {
  return (p.params as QueryParams) ?? {};
}

function filterRows(q: QueryParams): ImageRow[] {
  let r = rows;
  if (q.minStars != null) r = r.filter((x) => x.stars >= q.minStars!);
  if (q.flag != null) r = r.filter((x) => x.flag === q.flag);
  if (q.colorLabel != null) {
    r = r.filter((x) =>
      q.colorLabel === LABEL_NONE
        ? x.colorLabel == null
        : x.colorLabel === q.colorLabel,
    );
  }
  if (q.search) {
    const s = q.search.toLowerCase();
    r = r.filter((x) => x.filename.toLowerCase().includes(s));
  }
  return r;
}

function sortRows(r: ImageRow[], sort: QueryParams["sort"]): ImageRow[] {
  const by = [...r];
  switch (sort) {
    case "filename":
      return by.sort((a, b) => a.filename.localeCompare(b.filename));
    case "filename_desc":
      return by.sort((a, b) => b.filename.localeCompare(a.filename));
    case "rating_desc":
      return by.sort((a, b) => b.stars - a.stars);
    case "rating_asc":
      return by.sort((a, b) => a.stars - b.stars);
    case "capture_asc":
      return by.sort((a, b) => (a.captureDate ?? 0) - (b.captureDate ?? 0));
    default:
      return by.sort((a, b) => (b.captureDate ?? 0) - (a.captureDate ?? 0));
  }
}

function queryRows(q: QueryParams): ImageRow[] {
  const filtered = sortRows(filterRows(q), q.sort);
  const offset = q.offset ?? 0;
  const limit = q.limit ?? filtered.length;
  return filtered.slice(offset, offset + limit);
}

// ── Cull mutations (persist across refresh so star/flag filters are demonstrable) ──

const num = (v: unknown): number =>
  typeof v === "number" ? v : Number(v) || 0;
const ids = (v: unknown): number[] => (Array.isArray(v) ? v.map(num) : []);
const flagOf = (v: unknown): ImageRow["flag"] =>
  v === "pick" || v === "reject" ? v : "none";
const rowById = (id: number) => rows.find((r) => r.id === id);

// ── Generated imagery ────────────────────────────────────────────────────────

function hashHue(hash: string): number {
  // Spread hues evenly across fixtures (hashes differ only in trailing digits).
  const n = parseInt(hash.replace(/\D/g, ""), 10) || hash.length;
  return (n * 47) % 360;
}

/** A deterministic SVG placeholder for a `thumb://localhost/<hash>?size=N` URL. */
function thumbPlaceholder(url: string): string {
  const hash = /thumb:\/\/[^/]+\/([^?]+)/.exec(url)?.[1] ?? "thumb";
  const size = Number(/[?&]size=(\d+)/.exec(url)?.[1] ?? 512);
  const w = size;
  const h = Math.round(size * (2 / 3));
  const hue = hashHue(hash);
  const label = hash.replace(/^mockhash/, "IMG ");
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}">\
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">\
<stop offset="0" stop-color="hsl(${hue} 55% 52%)"/>\
<stop offset="1" stop-color="hsl(${(hue + 40) % 360} 50% 30%)"/>\
</linearGradient></defs>\
<rect width="100%" height="100%" fill="url(#g)"/>\
<text x="50%" y="50%" fill="rgba(255,255,255,0.92)" font-family="monospace" \
font-size="${Math.round(size * 0.09)}" text-anchor="middle" dominant-baseline="middle">${label}</text>\
</svg>`;
  // Escape parens too: thumbnails are used in unquoted CSS `url(...)`, where the `)` inside the
  // SVG's `hsl(...)` colors (which encodeURIComponent leaves intact) would close the url early.
  const enc = encodeURIComponent(svg)
    .replace(/\(/g, "%28")
    .replace(/\)/g, "%29");
  return `data:image/svg+xml,${enc}`;
}

function dataUrlToArrayBuffer(dataUrl: string): ArrayBuffer {
  const b64 = dataUrl.slice(dataUrl.indexOf(",") + 1);
  const bin = atob(b64);
  const u = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i);
  return u.buffer;
}

/** A canvas-rendered JPEG whose look responds to exposure/temp/tint, returned as bytes. */
function makeDevelopJpeg(
  params: Record<string, unknown> | undefined,
): ArrayBuffer {
  const w = 1200;
  const h = 800;
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d");
  if (!ctx) return new ArrayBuffer(0);
  const exposure = num(params?.exposure);
  const temp = num(params?.temp);
  const tint = num(params?.tint);
  const light = Math.max(4, Math.min(96, 52 + exposure * 12));
  const warm = Math.max(-1, Math.min(1, temp / 100));
  const g = ctx.createLinearGradient(0, 0, w, h);
  g.addColorStop(0, `hsl(${210 - warm * 60} 45% ${light}%)`);
  g.addColorStop(
    1,
    `hsl(${Math.max(0, 30 + tint)} 50% ${Math.max(6, light - 26)}%)`,
  );
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, w, h);
  ctx.fillStyle = "rgba(255,255,255,0.16)";
  ctx.beginPath();
  ctx.arc(w * 0.5, h * 0.46, 190, 0, Math.PI * 2);
  ctx.fill();
  ctx.fillStyle = "rgba(0,0,0,0.55)";
  ctx.font = "30px sans-serif";
  ctx.fillText("MOCK DEVELOP RENDER", 40, 56);
  ctx.fillText(
    `exposure ${exposure.toFixed(2)}   temp ${temp.toFixed(0)}   tint ${tint.toFixed(0)}`,
    40,
    h - 36,
  );
  return dataUrlToArrayBuffer(c.toDataURL("image/jpeg", 0.82));
}

/**
 * Raw-RGBA response for `develop_render` (new viewport model).
 * Layout: [outW u32 LE][outH u32 LE][rgba8 outW*outH*4]
 * The gradient shifts with view.ox/oy so panning is visually confirmed.
 */
function makeDevelopRgba(p: Record<string, unknown>): ArrayBuffer {
  const view = (p.view ?? {}) as {
    ox?: number;
    oy?: number;
    sx?: number;
    sy?: number;
  };
  const ox = num(view.ox ?? 0);
  const oy = num(view.oy ?? 0);
  const outW = Math.max(1, Math.min(4096, num(p.outW ?? 1200)));
  const outH = Math.max(1, Math.min(4096, num(p.outH ?? 800)));
  const params = (p.params ?? {}) as Record<string, unknown>;
  const exposure = num(params.exposure);
  const temp = num(params.temp);

  // 8-byte header
  const buf = new ArrayBuffer(8 + outW * outH * 4);
  const header = new DataView(buf, 0, 8);
  header.setUint32(0, outW, true);
  header.setUint32(4, outH, true);

  const pixels = new Uint8Array(buf, 8);
  const light = Math.max(20, Math.min(235, 128 + Math.round(exposure * 30)));
  const warmShift = Math.round(temp * 0.4);

  for (let y = 0; y < outH; y++) {
    for (let x = 0; x < outW; x++) {
      const i = (y * outW + x) * 4;
      // Gradient that shifts visibly with pan (ox/oy) so panning is testable
      const fx = (x / outW + ox) % 1;
      const fy = (y / outH + oy) % 1;
      pixels[i + 0] = Math.round(light + warmShift + fx * 60) & 0xff; // R
      pixels[i + 1] = Math.round(light + fy * 40) & 0xff; // G
      pixels[i + 2] = Math.round(light - warmShift + (1 - fx) * 40) & 0xff; // B
      pixels[i + 3] = 255; // A
    }
  }
  return buf;
}

function makeHistogram(): { r: number[]; g: number[]; b: number[] } {
  const bell = (center: number): number[] =>
    Array.from({ length: 256 }, (_, i) =>
      Math.round(1000 * Math.exp(-((i - center) ** 2) / (2 * 42 * 42))),
    );
  return { r: bell(118), g: bell(128), b: bell(138) };
}

// ── Command handlers ─────────────────────────────────────────────────────────

const STATS = {
  scanned: FIXTURE_COUNT,
  added: FIXTURE_COUNT,
  skipped: 0,
  failed: 0,
};

const HANDLERS: Record<string, (p: Record<string, unknown>) => unknown> = {
  // Library
  library_query: (p) => queryRows(getParams(p)),
  library_count: (p) => filterRows(getParams(p)).length,
  library_folders: () => [
    { id: 1, path: `${LIB_ROOT}/2026`, count: FIXTURE_COUNT },
  ],
  image_meta: (p) => rowById(num(p.id)) ?? null,
  library_index_root: () => STATS,
  database_reset: () => STATS,
  app_default_library: () => LIB_ROOT,
  app_library_root: () => LIB_ROOT,

  // Import / dedup
  import_start: () => ({
    sessionId: 1,
    total: 0,
    added: 0,
    skipped: 0,
    failed: 0,
  }),
  dedup_scan: () => [],
  dedup_scan_perceptual: () => [],
  dedup_resolve: () => 0,
  dedup_resolve_bulk: () => 0,

  // Thumb cache settings
  thumb_cache_cap: () => 2 * 1024 * 1024 * 1024,
  thumb_cache_size: () => 128 * 1024 * 1024,
  set_thumb_cache_cap: () => 0,

  // Cull (persisted to fixtures)
  cull_set_rating: (p) => {
    const r = rowById(num(p.imageId));
    if (r) r.stars = num(p.stars);
  },
  cull_set_flag: (p) => {
    const r = rowById(num(p.imageId));
    if (r) r.flag = flagOf(p.flag);
  },
  cull_set_label: (p) => {
    const r = rowById(num(p.imageId));
    if (r) r.colorLabel = p.label == null ? null : String(p.label);
  },
  cull_set_rating_many: (p) =>
    ids(p.imageIds).forEach((id) => {
      const r = rowById(id);
      if (r) r.stars = num(p.stars);
    }),
  cull_set_flag_many: (p) =>
    ids(p.imageIds).forEach((id) => {
      const r = rowById(id);
      if (r) r.flag = flagOf(p.flag);
    }),
  cull_set_label_many: (p) =>
    ids(p.imageIds).forEach((id) => {
      const r = rowById(id);
      if (r) r.colorLabel = p.label == null ? null : String(p.label);
    }),

  // Keywords
  keywords_list: () => [
    { id: 1, name: "landscape", count: 12 },
    { id: 2, name: "portrait", count: 7 },
    { id: 3, name: "wildlife", count: 4 },
  ],
  keywords_for_image: () => [],
  keyword_add_to_image: (p) => ({
    id: 99,
    name: String(p.name ?? "tag"),
    count: 1,
  }),
  keyword_add_to_images: (p) => ({
    id: 99,
    name: String(p.name ?? "tag"),
    count: 1,
  }),
  keyword_remove_from_image: () => undefined,
  keyword_delete: () => undefined,

  // Collections
  collections_list: () => [
    { id: 1, name: "Best of 2026", isSmart: false, query: null, count: 9 },
    {
      id: 2,
      name: "5 stars",
      isSmart: true,
      query: '{"minStars":5}',
      count: 8,
    },
  ],
  collections_for_image: () => [],
  collection_create: () => 3,
  collection_rename: () => undefined,
  collection_delete: () => undefined,
  collection_add_images: (p) => ids(p.imageIds).length,
  collection_remove_images: (p) => ids(p.imageIds).length,

  // Develop
  develop_get_edit: () => structuredClone(DEFAULT_PARAMS),
  develop_set_edit: () => undefined,
  // New viewport model: returns [outW u32 LE][outH u32 LE][rgba8 outW*outH*4]
  develop_render: (p) => makeDevelopRgba(p),
  develop_preview_jpeg: () => makeDevelopJpeg(undefined),
  loupe_jpeg: () => makeDevelopJpeg(undefined),
  develop_get_histogram: () => makeHistogram(),
  develop_regen_thumb: () => Date.now(),
  export_image: () => undefined,

  // AI analysis
  analysis_status: () => ({
    total: FIXTURE_COUNT,
    analyzed: 0,
    pending: FIXTURE_COUNT,
    modelsReady: false,
    running: false,
  }),
  analysis_models_ensure: () => undefined,
  analysis_run: () => ({ analyzed: 0, failed: 0 }),
  analysis_cancel: () => undefined,
  features_backfill: () => 0,
  analysis_facets: () => [],
  image_detections: () => [],
  image_caption: () => null,
  image_user_labels: () => ({ containsPerson: null, containsAnimal: null }),
  set_image_user_label: () => undefined,
  set_image_user_label_many: () => undefined,
  analysis_detector_size: () => 640,
  set_analysis_detector_size: () => undefined,

  // Faces / People
  faces_status: () => ({
    total: FIXTURE_COUNT,
    processed: FIXTURE_COUNT,
    pending: 0,
    modelsReady: true,
    running: false,
    faces: 7,
    people: 3,
  }),
  faces_models_ensure: () => undefined,
  faces_run: () => ({
    images: FIXTURE_COUNT,
    faces: 7,
    cluster: { assigned: 6, newPeople: 2, deferred: 1 },
  }),
  faces_cancel: () => undefined,
  people_list: () => [
    {
      id: 1,
      name: "Ada Lovelace",
      hidden: false,
      faceCount: 4,
      coverFaceId: 11,
      coverImageHash: "mockhash3",
      coverBbox: [0.36, 0.18, 0.64, 0.52],
    },
    {
      id: 2,
      name: "Alan Turing",
      hidden: false,
      faceCount: 3,
      coverFaceId: 21,
      coverImageHash: "mockhash7",
      coverBbox: [0.4, 0.22, 0.66, 0.56],
    },
    {
      id: 3,
      name: null,
      hidden: false,
      faceCount: 2,
      coverFaceId: 31,
      coverImageHash: "mockhash12",
      coverBbox: [0.3, 0.2, 0.58, 0.5],
    },
  ],
  person_faces: (p) => {
    const pid = num(p.personId);
    const hash =
      pid === 1 ? "mockhash3" : pid === 2 ? "mockhash7" : "mockhash12";
    return [
      {
        id: pid * 10 + 1,
        imageId: 3,
        imageHash: hash,
        bbox: [0.36, 0.18, 0.64, 0.52],
        status: "confirmed",
        detScore: 0.93,
        quality: 1.2e7,
      },
      {
        id: pid * 10 + 2,
        imageId: 8,
        imageHash: "mockhash8",
        bbox: [0.42, 0.24, 0.68, 0.58],
        status: "unconfirmed",
        detScore: 0.81,
        quality: 9e6,
      },
      {
        id: pid * 10 + 3,
        imageId: 15,
        imageHash: "mockhash15",
        bbox: [0.3, 0.2, 0.55, 0.5],
        status: "unconfirmed",
        detScore: 0.77,
        quality: 7e6,
      },
    ];
  },
  image_faces: () => [
    {
      id: 1,
      personId: 1,
      personName: "Ada Lovelace",
      bbox: [0.36, 0.18, 0.64, 0.52],
      status: "confirmed",
    },
    {
      id: 2,
      personId: null,
      personName: null,
      bbox: [0.7, 0.3, 0.86, 0.6],
      status: "unconfirmed",
    },
  ],
  person_set_name: () => undefined,
  person_set_hidden: () => undefined,
  person_set_cover: () => undefined,
  person_merge: () => undefined,
  face_confirm: () => undefined,
  face_reject: () => undefined,
  face_assign: () => undefined,
  faces_delete_all: () => undefined,
};

function handle(cmd: string, payload?: InvokeArgs): unknown {
  const h = HANDLERS[cmd];
  if (h) return h((payload ?? {}) as Record<string, unknown>);
  if (cmd.startsWith("plugin:")) return undefined; // dialogs/window/opener/events: no-op
  console.warn(`[tauriMock] unhandled command: ${cmd}`);
  return null;
}

// ── Entry point ──────────────────────────────────────────────────────────────

declare global {
  interface Window {
    /** Dev hook read by `thumbUrl()` in ipc.ts to substitute placeholders for `thumb://` URLs. */
    __darkroomThumbMock?: (url: string) => string;
  }
}

export function installTauriMock(): void {
  if ("__TAURI_INTERNALS__" in window) return; // real Tauri runtime — do nothing
  mockWindows("main");
  mockConvertFileSrc("macos");
  // Grid thumbnails and the loupe build `thumb://` URLs, which have no protocol handler in a
  // plain browser; thumbUrl() reads this hook in dev to serve generated placeholders instead.
  window.__darkroomThumbMock = thumbPlaceholder;
  mockIPC((cmd, payload) => handle(cmd, payload), { shouldMockEvents: true });
  console.info(
    `[tauriMock] active — mock Tauri backend installed (${FIXTURE_COUNT} fixture images). Browser test mode.`,
  );
}
