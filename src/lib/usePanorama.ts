import { useCallback, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store/app";
import { panoramaMerge, panoramaCancel, type PanoramaOptions } from "./ipc";
import { log } from "./logger";

/** Human-readable label for each backend merge phase (`panorama:progress` `phase`). Falls back to the
 *  raw phase string for a future phase this build doesn't know about yet. */
const PANORAMA_PHASE_LABELS: Record<string, string> = {
  register: "Aligning images",
  bundle_adjust: "Refining alignment",
  warp: "Projecting",
  blend: "Blending",
  crop: "Cropping",
  rectangle: "Boundary warp",
  encode: "Writing DNG",
};

export function panoramaPhaseLabel(phase: string): string {
  return PANORAMA_PHASE_LABELS[phase] ?? phase;
}

/** True when `err` looks like the panorama IPC commands aren't wired up on the backend yet (or are
 *  blocked by the Tauri ACL) rather than a real merge failure — surfaced as a friendly "merge engine
 *  not available" state instead of a raw error. Tauri v2 rejects an invoke for an unregistered command
 *  with a message of the shape `command <name> not found`; a missing capability grant rejects with
 *  wording like `... not allowed. Command not found in the config ACL ...` — both are treated the same
 *  here since, from the user's perspective, the merge engine simply isn't available yet. */
export function isMergeEngineUnavailable(err: unknown): boolean {
  const msg = typeof err === "string" ? err : err instanceof Error ? err.message : String(err);
  return /\bcommand\b[^]*\bnot found\b/i.test(msg) || /not allowed\b.*\bacl\b/i.test(msg);
}

/** Friendly message for a failed `panoramaPreview`/`panoramaMerge` call. */
export function panoramaErrorMessage(err: unknown): string {
  if (isMergeEngineUnavailable(err)) return "merge engine not available";
  return typeof err === "string" ? err : err instanceof Error ? err.message : String(err);
}

// The `panorama:*` event listeners are process-wide singletons: set up exactly once no matter how
// many components call `usePanorama()` (PanoramaModal, PanoramaPill, …). A module-level guard (rather
// than a per-instance ref, as `useAnalysis`/`useModelDownloadListeners` use for their single call
// site) is needed here because this hook is meant to be called from multiple places.
let listenersBootstrapped = false;

function bootstrapListeners(): void {
  if (listenersBootstrapped) return;
  listenersBootstrapped = true;

  void listen<{ phase: string }>("panorama:progress", (ev) => {
    useAppStore.getState().setPanoramaJob({ phase: ev.payload.phase });
  });

  void listen<{ imageId: number }>("panorama:done", (ev) => {
    useAppStore.getState().setPanoramaJob(null);
    useAppStore.getState().setToast("Panorama merged");
    // The backend also emits `library:changed` when it commits the new image, which `useLibrary`
    // already refreshes the grid on — here we just carry the freshly-merged image into the selection.
    useAppStore.getState().setSelection([ev.payload.imageId], ev.payload.imageId);
  });

  void listen<{ message: string }>("panorama:error", (ev) => {
    useAppStore.getState().setPanoramaJob(null);
    useAppStore
      .getState()
      .setToast(`Panorama merge failed: ${ev.payload.message}`);
  });
}

export interface PanoramaActions {
  /** Kick off a full merge. Sets the store job immediately (optimistic "register" phase) and resolves
   *  once the backend either finishes or fails — callers that want to close the modal right away
   *  should NOT await this; fire it and move on (the PanoramaPill takes over from the store job). */
  startMerge: (opts: PanoramaOptions) => Promise<void>;
  cancelMerge: () => Promise<void>;
}

/**
 * Owns the panorama merge job: `panorama:progress`/`panorama:done`/`panorama:error` event
 * subscriptions (bootstrapped once, however many components call this hook), starting a merge,
 * cancelling one, and the live job read from the store. Call from PanoramaModal (to start a merge)
 * and PanoramaPill (to read the job + cancel) — consistent with how `useAnalysis` is called wherever
 * its state/actions are needed, except this one is safe to call from more than one place.
 */
export function usePanorama(): { job: { phase: string } | null } & PanoramaActions {
  const job = useAppStore((s) => s.panoramaJob);

  useEffect(() => {
    bootstrapListeners();
  }, []);

  const startMerge = useCallback(async (opts: PanoramaOptions) => {
    const { setPanoramaJob, setToast } = useAppStore.getState();
    setPanoramaJob({ phase: "register" });
    try {
      await panoramaMerge(opts);
      // Happy path: `panorama:done` already clears the job, toasts, and selects the new image.
    } catch (err) {
      setPanoramaJob(null);
      setToast(`Panorama merge failed: ${panoramaErrorMessage(err)}`);
      log.warn("panorama", "merge failed", log.errorSummary(err));
    }
  }, []);

  const cancelMerge = useCallback(async () => {
    try {
      await panoramaCancel();
    } catch (err) {
      log.debug("panorama", "cancel failed", log.errorSummary(err));
    } finally {
      useAppStore.getState().setPanoramaJob(null);
    }
  }, []);

  return { job, startMerge, cancelMerge };
}
