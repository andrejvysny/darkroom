import type { DupGroup, DupImage } from "../../lib/ipc";

// ── Category presentation ────────────────────────────────────────────────────

export const CATEGORY_LABELS: Record<string, string> = {
  byte: "Exact duplicate",
  capture: "Same capture",
  perceptual: "Similar scene",
};

/** Short ALL-CAPS badge label shown next to a group. */
export const CATEGORY_BADGE: Record<string, string> = {
  byte: "EXACT",
  capture: "CAPTURE",
  perceptual: "SIMILAR",
};

export const CATEGORY_DOT: Record<string, string> = {
  byte: "var(--color-accent)",
  capture: "var(--color-star)",
  perceptual: "var(--color-t3)",
};

// ── Formatting ───────────────────────────────────────────────────────────────

export function fmtBytes(b: number): string {
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(0)} KB`;
  return `${(b / (1024 * 1024)).toFixed(1)} MB`;
}

export function fmtDate(ts: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

export function fmtExif(img: DupImage): string {
  const parts: string[] = [];
  if (img.iso != null) parts.push(`ISO ${img.iso}`);
  if (img.shutter) parts.push(img.shutter);
  if (img.aperture != null) parts.push(`f/${img.aperture.toFixed(1)}`);
  return parts.join(" · ") || "—";
}

/** Filename without its extension — used as the group's representative title. */
export function stem(filename: string): string {
  const dot = filename.lastIndexOf(".");
  return dot > 0 ? filename.slice(0, dot) : filename;
}

export function groupTitle(group: DupGroup): string {
  return stem(group.images[0]?.filename ?? group.key);
}

/** Total bytes reclaimable if every non-keeper in the group is trashed. */
export function reclaimableBytes(group: DupGroup, keeperId: number): number {
  return group.images
    .filter((i) => i.id !== keeperId)
    .reduce((sum, i) => sum + i.fileSize, 0);
}

// ── Group ordering ────────────────────────────────────────────────────────────

export type SortKey = "reclaim" | "files" | "name";
export type SortDir = "desc" | "asc";

export const SORT_LABELS: Record<SortKey, string> = {
  reclaim: "Reclaimable",
  files: "File count",
  name: "Name",
};

/** Stable copy of `groups` ordered by the chosen key/direction. `desc` = most/Z→A first. */
export function sortGroups(
  groups: DupGroup[],
  keepers: Record<string, number>,
  key: SortKey,
  dir: SortDir,
): DupGroup[] {
  const metric = (g: DupGroup): number | string => {
    if (key === "files") return g.images.length;
    if (key === "name") return groupTitle(g).toLowerCase();
    return reclaimableBytes(g, keepers[g.key] ?? suggestKeeper(g.images));
  };
  const sign = dir === "asc" ? 1 : -1;
  return [...groups].sort((a, b) => {
    const va = metric(a);
    const vb = metric(b);
    const c =
      typeof va === "string"
        ? va.localeCompare(vb as string)
        : (va as number) - (vb as number);
    return c !== 0 ? sign * c : a.key.localeCompare(b.key); // stable tiebreak
  });
}

// ── Keeper suggestion ─────────────────────────────────────────────────────────
// The suggested keeper is the highest keeper-fit score so the ranking the UI shows
// (the score ring) never contradicts which photo is recommended.

export function suggestKeeper(images: DupImage[]): number {
  return images.reduce((best, img) => {
    const s = keeperScoreIn(img, images);
    const bs = keeperScoreIn(best, images);
    if (s !== bs) return s > bs ? img : best;
    return img.id < best.id ? img : best; // stable tiebreak
  }).id;
}

export function defaultKeepers(groups: DupGroup[]): Record<string, number> {
  const out: Record<string, number> = {};
  for (const g of groups) out[g.key] = suggestKeeper(g.images);
  return out;
}

// ── Keeper-fit score (heuristic over REAL fields — no fabricated quality data) ─

function norm(v: number, lo: number, hi: number): number {
  if (hi <= lo) return 1;
  return Math.max(0, Math.min(1, (v - lo) / (hi - lo)));
}

interface GroupRanges {
  sizeLo: number;
  sizeHi: number;
  isoLo: number;
  isoHi: number;
}

function ranges(images: DupImage[]): GroupRanges {
  const sizes = images.map((i) => i.fileSize);
  const isos = images.map((i) => i.iso ?? 0).filter((v) => v > 0);
  return {
    sizeLo: Math.min(...sizes),
    sizeHi: Math.max(...sizes),
    isoLo: isos.length ? Math.min(...isos) : 0,
    isoHi: isos.length ? Math.max(...isos) : 0,
  };
}

/** 0–100 "keeper fit" derived from size (more data), ISO (lower = cleaner) and rating.
 *  Transparent heuristic over REAL fields — not a fabricated image-quality metric. */
function keeperScoreIn(img: DupImage, images: DupImage[]): number {
  const r = ranges(images);
  const sizeN = norm(img.fileSize, r.sizeLo, r.sizeHi);
  const isoN =
    img.iso != null && r.isoHi > r.isoLo
      ? 1 - norm(img.iso, r.isoLo, r.isoHi)
      : 0.5;
  const starN = img.stars / 5;
  const score = 0.4 * sizeN + 0.35 * isoN + 0.25 * starN;
  return Math.round(score * 100);
}

export function keeperScore(img: DupImage, group: DupGroup): number {
  return keeperScoreIn(img, group.images);
}

export function scoreColor(score: number): string {
  if (score >= 75) return "var(--color-pick)";
  if (score >= 50) return "var(--color-star)";
  return "var(--color-reject)";
}

// ── Decision signals (this frame vs the keeper) ───────────────────────────────

export interface Signal {
  key: string;
  label: string;
  valLabel: string;
  /** 0–1 bar fill within the group. */
  barFrac: number;
  /** Comparison vs keeper; null when there is nothing to compare. */
  delta: { label: string; good: boolean } | null;
}

export function decisionSignals(
  img: DupImage,
  keeper: DupImage,
  group: DupGroup,
): Signal[] {
  const r = ranges(group.images);
  const isKeeper = img.id === keeper.id;

  const sizeDeltaMb = (img.fileSize - keeper.fileSize) / (1024 * 1024);
  const isoDelta =
    img.iso != null && keeper.iso != null ? img.iso - keeper.iso : null;
  const starDelta = img.stars - keeper.stars;

  return [
    {
      key: "size",
      label: "File size",
      valLabel: fmtBytes(img.fileSize),
      barFrac: norm(img.fileSize, r.sizeLo, r.sizeHi),
      delta: isKeeper
        ? null
        : {
            label: `${sizeDeltaMb >= 0 ? "+" : ""}${sizeDeltaMb.toFixed(1)} MB`,
            good: img.fileSize >= keeper.fileSize,
          },
    },
    {
      key: "iso",
      label: "ISO",
      valLabel: img.iso != null ? `${img.iso}` : "—",
      barFrac:
        img.iso != null && r.isoHi > r.isoLo
          ? 1 - norm(img.iso, r.isoLo, r.isoHi)
          : 0.5,
      delta:
        isKeeper || isoDelta == null
          ? null
          : {
              label: `${isoDelta >= 0 ? "+" : ""}${isoDelta}`,
              good: isoDelta <= 0,
            },
    },
    {
      key: "stars",
      label: "Rating",
      valLabel: img.stars > 0 ? "★".repeat(img.stars) : "—",
      barFrac: img.stars / 5,
      delta:
        isKeeper || starDelta === 0
          ? null
          : {
              label: `${starDelta > 0 ? "+" : ""}${starDelta}★`,
              good: starDelta >= 0,
            },
    },
  ];
}
