import { save, open } from "@tauri-apps/plugin-dialog";
import { developGetEdit, exportImage } from "./ipc";
import { useAppStore } from "../store/app";

/**
 * Export a single image: load its saved develop params, prompt for a destination,
 * render full-resolution on the GPU, and write PNG/JPEG (inferred from the chosen extension).
 */
export async function runExport(
  imageId: number | null,
  filename?: string,
): Promise<void> {
  if (imageId == null) {
    useAppStore.getState().setToast("Select a photo to export");
    return;
  }
  const setToast = useAppStore.getState().setToast;
  try {
    const params = await developGetEdit(imageId);
    const base = (filename ?? `image-${imageId}`).replace(/\.[^.]+$/, "");
    const dest = await save({
      defaultPath: `${base}.jpg`,
      filters: [
        { name: "JPEG", extensions: ["jpg", "jpeg"] },
        { name: "PNG", extensions: ["png"] },
      ],
    });
    if (!dest) return;
    const format: "png" | "jpeg" = dest.toLowerCase().endsWith(".png")
      ? "png"
      : "jpeg";
    setToast("Exporting…");
    await exportImage(imageId, params, format, dest);
    setToast(`Exported → ${dest.split("/").pop()}`);
  } catch (err) {
    setToast(`Export failed: ${String(err)}`);
  }
}

/**
 * Batch-export several images as full-resolution JPEGs into a chosen folder, named after each
 * original file. Each runs through its saved develop params on the GPU.
 */
export async function runBatchExport(
  items: { id: number; filename: string }[],
): Promise<void> {
  const setToast = useAppStore.getState().setToast;
  if (items.length === 0) {
    setToast("Select photos to export");
    return;
  }
  const dir = await open({ directory: true, title: "Export selected to folder" });
  if (!dir) return;

  let done = 0;
  let failed = 0;
  for (const { id, filename } of items) {
    try {
      const params = await developGetEdit(id);
      const base = filename.replace(/\.[^.]+$/, "");
      const dest = `${dir}/${base}.jpg`;
      await exportImage(id, params, "jpeg", dest);
      done += 1;
    } catch {
      failed += 1;
    }
    setToast(`Exporting ${done + failed} / ${items.length}…`);
  }
  setToast(
    failed > 0
      ? `Exported ${done}, ${failed} failed → ${String(dir).split("/").pop()}`
      : `Exported ${done} → ${String(dir).split("/").pop()}`,
  );
}
