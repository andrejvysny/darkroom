import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  panoDetectStatus,
  panoDetectGroups,
  panoDetectRun,
  panoDetectCancel,
  panoDetectDismiss,
  panoDetectMarkMerged,
  type PanoGroupRow,
} from "./ipc";
import { useAppStore, type PanoDetectProgress, type PanoDetectStoreState } from "../store/app";
import { log } from "./logger";

export type { PanoDetectProgress };

export interface PanoDetectActions {
  detect: (force?: boolean) => Promise<void>;
  cancel: () => Promise<void>;
  reload: () => Promise<void>;
  dismiss: (groupId: number, dismissed: boolean) => Promise<void>;
  /** Stage a detected group into the existing Panorama merge modal: sets the merge source ids
   *  plus the originating group id (threaded through the merge IPC call and echoed back on
   *  `panorama:done`, where this module marks the group merged), which opens `PanoramaModal`. */
  openMerge: (group: PanoGroupRow) => void;
}

/** Human-readable label for each backend detect phase (`pano_detect:progress` `phase`). */
function progressLabel(p: NonNullable<PanoDetectProgress>): string {
  if (p.phase === "cluster") return "Clustering…";
  if (p.phase === "verify")
    return p.total > 0 ? `Verifying ${p.done}/${p.total}…` : "Verifying…";
  if (p.phase === "save") return "Saving…";
  return `${p.phase}…`;
}

async function reloadStatus(): Promise<void> {
  try {
    const status = await panoDetectStatus();
    useAppStore
      .getState()
      .setPanoDetect({ running: status.running, suggested: status.suggested });
  } catch (err) {
    log.debug("panoDetect", "reload status failed", log.errorSummary(err));
  }
}

async function reloadGroups(): Promise<void> {
  try {
    // Always fetch dismissed groups too — PanoSuggestions filters the "Show dismissed" toggle
    // client-side so toggling it doesn't need a round-trip.
    const groups = await panoDetectGroups(true);
    useAppStore.getState().setPanoDetect({ groups });
  } catch (err) {
    log.debug("panoDetect", "reload groups failed", log.errorSummary(err));
  }
}

async function reload(): Promise<void> {
  useAppStore.getState().setPanoDetect({ loading: true });
  await Promise.all([reloadStatus(), reloadGroups()]);
  useAppStore.getState().setPanoDetect({ loading: false });
}

/** Refresh the shared store (groups + suggested count) from the backend, for callers outside this
 *  module that change suggestion state through some other path. */
export function refreshPanoDetect(): void {
  void reload();
}

async function detect(force = false): Promise<void> {
  const { setPanoDetect, setToast } = useAppStore.getState();
  // Optimistic: flip busy immediately (mirrors the old per-instance optimistic progress) so every
  // consumer's Detect button dims right away instead of waiting for the first progress event.
  setPanoDetect({ running: true, progress: { phase: "cluster", done: 0, total: 0 } });
  try {
    await panoDetectRun(force);
    // Happy path: pano_detect:done (listener below) clears running/progress, reloads, and toasts.
  } catch (err) {
    setPanoDetect({ running: false, progress: null });
    const msg = err instanceof Error ? err.message : String(err);
    setToast(`Panorama detection failed: ${msg}`);
    log.warn("panoDetect", "run failed", { force, ...log.errorSummary(err) });
  }
}

async function cancel(): Promise<void> {
  try {
    await panoDetectCancel();
  } catch (err) {
    log.debug("panoDetect", "cancel failed", log.errorSummary(err));
  } finally {
    useAppStore.getState().setPanoDetect({ running: false, progress: null });
  }
}

async function dismiss(groupId: number, dismissed: boolean): Promise<void> {
  const { panoDetect, setPanoDetect } = useAppStore.getState();
  // Optimistic local update so the row flips immediately.
  setPanoDetect({
    groups: panoDetect.groups.map((g) =>
      g.id === groupId ? { ...g, status: dismissed ? "dismissed" : "suggested" } : g,
    ),
  });
  try {
    await panoDetectDismiss(groupId, dismissed);
    void reloadStatus();
  } catch (err) {
    log.warn("panoDetect", "dismiss failed", log.errorSummary(err));
    await reload(); // revert to server truth on failure
  }
}

function openMerge(group: PanoGroupRow): void {
  useAppStore.getState().setPanoramaSources({
    ids: group.members.map((m) => m.imageId),
    detectGroupId: group.id,
  });
}

// The `pano_detect:*` event listeners (+ the initial fetch) are process-wide singletons: set up
// exactly once no matter how many components call `usePanoDetect()` (LeftNav, PanoSuggestions, …).
// Mirrors `usePanorama.ts`'s `listenersBootstrapped` module guard — needed here for the same reason:
// this hook is called from more than one always-live place, and per-instance listeners meant
// duplicate `pano_detect:done`/`error` toasts, doubled status/groups round-trips on mount, and a
// LeftNav badge that could go stale after a dismiss/merge handled by a different instance.
let listenersBootstrapped = false;

function bootstrapListeners(): void {
  if (listenersBootstrapped) return;
  listenersBootstrapped = true;

  void reload();

  void listen<{ phase: string; done: number; total: number }>(
    "pano_detect:progress",
    (ev) => {
      useAppStore.getState().setPanoDetect({
        running: true,
        progress: {
          phase: ev.payload.phase,
          done: ev.payload.done,
          total: ev.payload.total,
        },
      });
    },
  );

  void listen<{ found: number }>("pano_detect:done", (ev) => {
    useAppStore.getState().setPanoDetect({ running: false, progress: null });
    void reload();
    useAppStore
      .getState()
      .setToast(
        ev.payload.found > 0
          ? `Found ${ev.payload.found} panorama suggestion${ev.payload.found === 1 ? "" : "s"}`
          : "No new panorama suggestions found",
      );
  });

  void listen<{ message: string }>("pano_detect:error", (ev) => {
    useAppStore.getState().setPanoDetect({ running: false, progress: null });
    useAppStore
      .getState()
      .setToast(`Panorama detection failed: ${ev.payload.message}`);
  });

  // A merge that began as a suggestion (PanoSuggestions → openMerge) carries its group id through
  // the merge IPC call; the backend echoes it back on `panorama:done`. Recording that merge is this
  // module's business, so it listens for the event directly rather than having `usePanorama` (or
  // the transport layer in `ipc.ts`) reach into the detect store: mark, then refresh so the group
  // leaves the review list immediately.
  void listen<{ imageId: number; detectGroupId: number | null }>(
    "panorama:done",
    (ev) => {
      const { imageId, detectGroupId } = ev.payload;
      if (detectGroupId == null) return;
      void panoDetectMarkMerged(detectGroupId, imageId)
        .then(reload)
        .catch((err: unknown) => {
          log.debug("panoDetect", "mark merged failed", log.errorSummary(err));
        });
    },
  );
}

/**
 * Panorama-detection state + actions, backed by the shared `panoDetect` store slice (see
 * `store/app.ts`). Event listeners and the initial status/groups fetch are module-level singletons
 * (bootstrapped once on first mount, however many call sites there are); the hook itself just reads
 * the store and hands back stable actions, so `LibraryView` (LeftNav's suggestion count),
 * `LeftNav` (the Detect/Re-detect header button), and `PanoSuggestions` (the full review overlay)
 * always see the exact same state.
 */
export function usePanoDetect(): PanoDetectStoreState & PanoDetectActions {
  const panoDetect = useAppStore((s) => s.panoDetect);

  useEffect(() => {
    bootstrapListeners();
  }, []);

  return {
    ...panoDetect,
    detect,
    cancel,
    reload,
    dismiss,
    openMerge,
  };
}

export { progressLabel as panoDetectProgressLabel };
