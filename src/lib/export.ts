import { save } from "@tauri-apps/plugin-dialog";
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
