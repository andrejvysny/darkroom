import { developGetEdit, exportImage, type ImageRow } from "./ipc";
import { useAppStore } from "../store/app";

/** One image queued for export. `captureDate`/`importedAt` feed the `{date}` token. */
export interface ExportTarget {
  id: number;
  filename: string;
  captureDate: number | null;
  importedAt: number;
}

export type ExportFormat = "jpeg" | "png";

export interface ExportOptions {
  /** Destination folder (absolute). */
  dir: string;
  /** Filename template with {original} {date} {seq} tokens (no extension). */
  template: string;
  format: ExportFormat;
  /** JPEG quality 1..100 (ignored for PNG). */
  quality: number;
}

export function toExportTarget(row: ImageRow): ExportTarget {
  return {
    id: row.id,
    filename: row.filename,
    captureDate: row.captureDate,
    importedAt: row.importedAt,
  };
}

/**
 * Open the Export modal for the given image ids (resolved against the shared library set). Used by
 * the TopBar button, command palette, ⌘⇧E and the Library selection bar. No-op toast if none resolve.
 */
export function openExport(ids: number[]): void {
  const st = useAppStore.getState();
  const byId = new Map(st.libraryImages.map((r) => [r.id, r]));
  const targets = ids
    .map((id) => byId.get(id))
    .filter((r): r is ImageRow => r != null)
    .map(toExportTarget);
  if (targets.length === 0) {
    st.setToast("Select a photo to export");
    return;
  }
  st.setExportTargets(targets);
}

/** epoch seconds → local `YYYY-MM-DD`. */
function ymd(epochSec: number): string {
  const d = new Date(epochSec * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

// Characters illegal in a filename on macOS/Windows → replaced with '_'.
const ILLEGAL = /[/\\:*?"<>|]/g;

/**
 * Resolve a filename template against one image's tokens, returning the base name (no extension),
 * sanitized. `{original}` = source basename sans ext, `{date}` = YYYY-MM-DD, `{seq}` = 1-based
 * zero-padded index.
 */
export function applyTemplate(
  template: string,
  vars: { original: string; dateStr: string; seq: number },
): string {
  const base = vars.original.replace(/\.[^.]+$/, "");
  const out = template
    .replace(/\{original\}/g, base)
    .replace(/\{date\}/g, vars.dateStr)
    .replace(/\{seq\}/g, String(vars.seq).padStart(3, "0"))
    .replace(ILLEGAL, "_")
    .trim();
  return out || base || "image";
}

/** The base name a target resolves to under `template` at position `seq` (1-based). For live preview. */
export function previewName(
  target: ExportTarget,
  template: string,
  seq: number,
): string {
  return applyTemplate(template, {
    original: target.filename,
    dateStr: ymd(target.captureDate ?? target.importedAt),
    seq,
  });
}

/**
 * Export a batch of images (1 or many) to `opts.dir` using their saved develop params. Names come
 * from the template; collisions are de-duplicated case-insensitively (macOS APFS default) with a
 * `-N` suffix. Toasts progress + result.
 */
export async function runExportBatch(
  targets: ExportTarget[],
  opts: ExportOptions,
): Promise<void> {
  const setToast = useAppStore.getState().setToast;
  if (targets.length === 0) {
    setToast("Select photos to export");
    return;
  }
  const ext = opts.format === "png" ? "png" : "jpg";
  const used = new Set<string>();
  let done = 0;
  let failed = 0;
  for (let i = 0; i < targets.length; i++) {
    const t = targets[i];
    try {
      const params = await developGetEdit(t.id);
      const name = previewName(t, opts.template, i + 1);
      let unique = name;
      for (let n = 1; used.has(unique.toLowerCase()); n++) unique = `${name}-${n}`;
      used.add(unique.toLowerCase());
      const dest = `${opts.dir}/${unique}.${ext}`;
      await exportImage(t.id, params, opts.format, dest, opts.quality);
      done += 1;
    } catch {
      failed += 1;
    }
    setToast(`Exporting ${done + failed} / ${targets.length}…`);
  }
  const where = opts.dir.split("/").pop();
  setToast(
    failed > 0
      ? `Exported ${done}, ${failed} failed → ${where}`
      : `Exported ${done} → ${where}`,
  );
}
