import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store/app";
import {
  appLibraryRoot,
  importStart,
  type ImportMode,
  type ImportStats,
} from "./ipc";

/**
 * Runs the interactive import flow:
 * 1. Pick a source directory
 * 2. Resolve or pick a destination (library root)
 * 3. Subscribe to progress/done events, toast updates
 * 4. Call importStart; on done call onComplete to refresh the library
 */
export async function runImport(
  mode: ImportMode = "copy",
  onComplete?: () => void,
  recursive = true,
): Promise<void> {
  const { setToast } = useAppStore.getState();

  const source = await open({
    directory: true,
    title: "Select import source (card / folder)",
  });
  if (!source) return;

  let dest = await appLibraryRoot();
  if (!dest) {
    const picked = await open({
      directory: true,
      title: "Select library destination",
    });
    if (!picked) return;
    dest = picked as string;
  }

  // Subscribe to events before invoking so we don't miss early emissions
  const unProgress = await listen<{ done: number; total: number }>(
    "import:progress",
    (ev) => {
      setToast(`Importing ${ev.payload.done} / ${ev.payload.total}…`);
    },
  );

  const unDone = await listen<ImportStats>("import:done", (ev) => {
    unProgress();
    unDone();
    const { added, skipped } = ev.payload;
    setToast(`Imported: added ${added}, skipped ${skipped}`);
    onComplete?.();
  });

  try {
    await importStart(source as string, mode, dest, recursive);
  } catch (err) {
    unProgress();
    unDone();
    setToast(`Import failed: ${String(err)}`);
  }
}
