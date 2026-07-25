import { useCallback, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../store/app";
import {
  panoramaMerge,
  panoramaCancel,
  panoramaStatus,
  type PanoramaOptions,
} from "./ipc";
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
  // Not a backend phase: the placeholder `reconnect` (below) uses it when all we know is that a
  // merge is running, because we joined after its last progress event.
  running: "in progress",
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

/** Bumped by every `panorama:*` event. `reconnect` compares it across its await so a merge that
 *  finished while the status call was in flight can't resurrect the pill. */
let eventSeq = 0;

function bootstrapListeners(): void {
  if (listenersBootstrapped) return;
  listenersBootstrapped = true;

  void listen<{ phase: string }>("panorama:progress", (ev) => {
    eventSeq += 1;
    useAppStore.getState().setPanoramaJob({ phase: ev.payload.phase });
  });

  void listen<{
    imageId: number;
    detectGroupId: number | null;
    used: number;
    total: number;
  }>("panorama:done", (ev) => {
    eventSeq += 1;
    const { imageId, used, total } = ev.payload;
    useAppStore.getState().setPanoramaJob(null);
    useAppStore
      .getState()
      .setToast(
        used < total
          ? `Panorama merged — stitched ${used} of ${total} frames (${total - used} didn't overlap)`
          : "Panorama merged",
      );
    // The backend also emits `library:changed` when it commits the new image, which `useLibrary`
    // already refreshes the grid on — here we just carry the freshly-merged image into the selection.
    useAppStore.getState().setSelection([imageId], imageId);

    // A merge handed off from a detected suggestion (PanoSuggestions → openMerge → PanoramaModal →
    // startMerge) carries its group id through the merge call, and the backend echoes it back here
    // — no ambient store field to go stale. Recording the merge is `usePanoDetect`'s job (it owns
    // both the IPC call and the store refresh); it listens for this same event.
  });

  void listen<{ message: string }>("panorama:error", (ev) => {
    eventSeq += 1;
    useAppStore.getState().setPanoramaJob(null);
    useAppStore
      .getState()
      .setToast(`Panorama merge failed: ${ev.payload.message}`);
  });
}

// One-shot per renderer, for the same reason `listenersBootstrapped` is: this hook has several call
// sites, and the probe below must fire exactly once no matter how many mount.
let statusProbed = false;

/**
 * Re-attach to a merge that is already running in the backend.
 *
 * A merge outlives the webview: reload the renderer (or open a fresh window) mid-merge and the store
 * comes back empty, so the PanoramaPill vanishes even though the job is still churning — and the
 * next event the UI would see is `panorama:done`. Asking `panorama_status` once at mount closes that
 * hole; the phase is a placeholder until the next `panorama:progress` event refines it.
 */
function reconnect(): void {
  if (statusProbed) return;
  statusProbed = true;
  const seenBefore = eventSeq;
  void panoramaStatus()
    .then((status) => {
      // A real event landed while we were asking — it knows better than this snapshot does.
      if (!status.running || eventSeq !== seenBefore) return;
      if (useAppStore.getState().panoramaJob) return;
      useAppStore.getState().setPanoramaJob({ phase: "running" });
    })
    .catch((err: unknown) => {
      log.debug("panorama", "status probe failed", log.errorSummary(err));
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
    // After the listeners, so a merge that finishes during the probe is seen as an event first.
    reconnect();
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
