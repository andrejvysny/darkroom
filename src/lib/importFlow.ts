import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store/app";
import {
  appLibraryRoot,
  importCommit,
  type ImportMode,
  type ImportOptions,
  type ImportStats,
} from "./ipc";

/** Open the native folder picker; returns the chosen path (or null if cancelled). */
export async function pickFolder(title: string): Promise<string | null> {
  const picked = await open({ directory: true, title });
  return typeof picked === "string" ? picked : null;
}

/** The configured library root, if any (the default copy/move destination). */
export function resolveDest(): Promise<string | null> {
  return appLibraryRoot();
}

/**
 * Commit a staged import: copy/move/reference only the `selected` source paths, then apply the
 * on-import `options`. Subscribes to progress/done toasts; calls `onComplete` to refresh the library.
 */
export async function commitImport(
  source: string,
  mode: ImportMode,
  dest: string,
  selected: string[],
  options: ImportOptions,
  onComplete?: () => void,
): Promise<void> {
  const { setToast } = useAppStore.getState();

  // Subscribe before invoking so we don't miss early emissions.
  const unProgress = await listen<{ done: number; total: number }>(
    "import:progress",
    (ev) => setToast(`Importing ${ev.payload.done} / ${ev.payload.total}…`),
  );
  const unDone = await listen<ImportStats>("import:done", (ev) => {
    unProgress();
    unDone();
    const { added, skipped, sourceRetained } = ev.payload;
    const retained =
      sourceRetained > 0
        ? `, ${sourceRetained} original(s) kept (trash failed)`
        : "";
    setToast(`Imported: added ${added}, skipped ${skipped}${retained}`);
    onComplete?.();
  });

  try {
    await importCommit(source, mode, dest, selected, options);
  } catch (err) {
    unProgress();
    unDone();
    setToast(`Import failed: ${String(err)}`);
  }
}
