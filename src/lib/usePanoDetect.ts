import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  panoDetectStatus,
  panoDetectGroups,
  panoDetectRun,
  panoDetectCancel,
  panoDetectDismiss,
  type PanoDetectStatus,
  type PanoGroupRow,
} from "./ipc";
import { useAppStore } from "../store/app";
import { log } from "./logger";

export type PanoDetectProgress = {
  phase: string;
  done: number;
  total: number;
} | null;

export interface PanoDetectState {
  status: PanoDetectStatus | null;
  groups: PanoGroupRow[];
  progress: PanoDetectProgress;
  error: string | null;
}

export interface PanoDetectActions {
  detect: (force?: boolean) => Promise<void>;
  cancel: () => Promise<void>;
  reload: () => Promise<void>;
  dismiss: (groupId: number, dismissed: boolean) => Promise<void>;
  /** Stage a detected group into the existing Panorama merge modal: sets the active-group id (so
   *  `usePanorama`'s `panorama:done` handler can call `panoDetectMarkMerged`) and the merge
   *  source ids, which opens `PanoramaModal`. */
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

/**
 * Owns panorama-detection state: scan status, the detected-group list, and live
 * progress/error — plus the actions to drive a scan and review its results. Modeled on
 * `useAnalysis`: each call site gets its own local state, bootstrapped once (StrictMode-safe via a
 * per-instance ref guard) and kept live by its own `pano_detect:*` event subscriptions, so both
 * `LibraryView` (LeftNav's suggestion count) and `PanoSuggestions` (the full review overlay) — the
 * two call sites — see consistent, independently-live state regardless of mount order.
 */
export function usePanoDetect(): PanoDetectState & PanoDetectActions {
  const [status, setStatus] = useState<PanoDetectStatus | null>(null);
  const [groups, setGroups] = useState<PanoGroupRow[]>([]);
  const [progress, setProgress] = useState<PanoDetectProgress>(null);
  const [error, setError] = useState<string | null>(null);

  // Guard against double-run in React 19 StrictMode
  const bootstrappedRef = useRef(false);

  const reloadStatus = useCallback(async () => {
    try {
      setStatus(await panoDetectStatus());
    } catch (err) {
      log.debug("panoDetect", "reload status failed", log.errorSummary(err));
    }
  }, []);

  const reloadGroups = useCallback(async () => {
    try {
      // Always fetch dismissed groups too — PanoSuggestions filters the "Show dismissed" toggle
      // client-side so toggling it doesn't need a round-trip.
      setGroups(await panoDetectGroups(true));
    } catch (err) {
      log.debug("panoDetect", "reload groups failed", log.errorSummary(err));
    }
  }, []);

  const reload = useCallback(async () => {
    await Promise.all([reloadStatus(), reloadGroups()]);
  }, [reloadStatus, reloadGroups]);

  const detect = useCallback(async (force = false) => {
    setError(null);
    // Optimistic progress, mirrors usePanorama's optimistic job set — pano_detect:progress
    // overwrites this as soon as the backend reports the real phase/counts.
    setProgress({ phase: "cluster", done: 0, total: 0 });
    try {
      await panoDetectRun(force);
      // Happy path: pano_detect:done (below) clears progress, reloads, and toasts.
    } catch (err) {
      setProgress(null);
      const msg = err instanceof Error ? err.message : String(err);
      setError(msg);
      useAppStore.getState().setToast(`Panorama detection failed: ${msg}`);
      log.warn("panoDetect", "run failed", { force, ...log.errorSummary(err) });
    }
  }, []);

  const cancel = useCallback(async () => {
    try {
      await panoDetectCancel();
    } catch (err) {
      log.debug("panoDetect", "cancel failed", log.errorSummary(err));
    } finally {
      setProgress(null);
    }
  }, []);

  const dismiss = useCallback(
    async (groupId: number, dismissed: boolean) => {
      // Optimistic local update so the row flips immediately.
      setGroups((prev) =>
        prev.map((g) =>
          g.id === groupId
            ? { ...g, status: dismissed ? "dismissed" : "suggested" }
            : g,
        ),
      );
      try {
        await panoDetectDismiss(groupId, dismissed);
        void reloadStatus();
      } catch (err) {
        log.warn("panoDetect", "dismiss failed", log.errorSummary(err));
        await reload(); // revert to server truth on failure
      }
    },
    [reload, reloadStatus],
  );

  const openMerge = useCallback((group: PanoGroupRow) => {
    useAppStore.getState().setActivePanoDetectGroup(group.id);
    useAppStore
      .getState()
      .setPanoramaSources(group.members.map((m) => m.imageId));
  }, []);

  // Mount: fetch initial status + groups, register event listeners
  useEffect(() => {
    if (bootstrappedRef.current) return;
    bootstrappedRef.current = true;

    void reload();

    const unlisteners: UnlistenFn[] = [];

    async function setup() {
      const unProgress = await listen<{
        phase: string;
        done: number;
        total: number;
      }>("pano_detect:progress", (ev) => {
        setProgress({
          phase: ev.payload.phase,
          done: ev.payload.done,
          total: ev.payload.total,
        });
      });
      unlisteners.push(unProgress);

      const unDone = await listen<{ found: number }>(
        "pano_detect:done",
        (ev) => {
          setProgress(null);
          setError(null);
          void reload();
          useAppStore
            .getState()
            .setToast(
              ev.payload.found > 0
                ? `Found ${ev.payload.found} panorama suggestion${ev.payload.found === 1 ? "" : "s"}`
                : "No new panorama suggestions found",
            );
        },
      );
      unlisteners.push(unDone);

      const unError = await listen<{ message: string }>(
        "pano_detect:error",
        (ev) => {
          setProgress(null);
          setError(ev.payload.message);
          useAppStore
            .getState()
            .setToast(`Panorama detection failed: ${ev.payload.message}`);
        },
      );
      unlisteners.push(unError);
    }

    void setup();

    return () => {
      unlisteners.forEach((fn) => fn());
    };
  }, [reload]);

  return {
    status,
    groups,
    progress,
    error,
    detect,
    cancel,
    reload,
    dismiss,
    openMerge,
  };
}

export { progressLabel as panoDetectProgressLabel };
