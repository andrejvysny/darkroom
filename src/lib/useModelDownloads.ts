import { useEffect, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  modelsOverview,
  modelsCancel,
  modelsRemove,
  analysisModelsEnsure,
  facesModelsEnsure,
  maskAiModelsEnsure,
  type ModelGroupId,
  type ModelDownloadEvent,
  type MaskAiTier,
} from "./ipc";
import { useModelsStore, type DownloadState } from "../store/models";
import { log } from "./logger";

/** Capability → its backend `<id>:models` progress event name. */
const EVENTS: ReadonlyArray<readonly [ModelGroupId, string]> = [
  ["analysis", "analysis:models"],
  ["faces", "faces:models"],
  ["mask_ai", "mask_ai:models"],
];

/** Backend `ensure` trigger per capability. */
const ENSURE: Record<ModelGroupId, () => Promise<void>> = {
  analysis: analysisModelsEnsure,
  faces: facesModelsEnsure,
  mask_ai: maskAiModelsEnsure,
};

const STARTING: DownloadState = {
  phase: "downloading",
  fileName: "",
  fileIndex: 0,
  fileCount: 0,
  bytesDone: 0,
  bytesTotal: 0,
  bytesPerSec: 0,
  etaSec: null,
  error: null,
};

// Per-group last sample for rate derivation. Module-level because the listener is single-instance.
const samples: Partial<
  Record<ModelGroupId, { bytes: number; t: number; bps: number }>
> = {};

// Groups whose download the user just cancelled. Late progress events (the backend stops within one
// ~100 ms chunk) are swallowed so the pill clears immediately and doesn't reappear. Auto-expires so a
// later (re)download works normally — including implicit ones (Analyze / first AI-mask click) that
// don't go through startModelDownload.
const cancelling = new Set<ModelGroupId>();

function isCancel(msg: string): boolean {
  return /cancel/i.test(msg);
}

/** Re-fetch the manager overview into the store. */
export async function refreshModelOverview(): Promise<void> {
  try {
    useModelsStore.getState().setOverview(await modelsOverview());
  } catch (err) {
    log.debug("models", "overview failed", log.errorSummary(err));
  }
}

/** Start (or resume) a capability's download. Progress arrives via events; this handles terminal
 *  state — clears on success, keeps an error entry on real failure, silent on user cancel. */
export async function startModelDownload(id: ModelGroupId): Promise<void> {
  const { setDownload } = useModelsStore.getState();
  cancelling.delete(id); // fresh download — stop swallowing events
  setDownload(id, { ...STARTING });
  try {
    await ENSURE[id]();
    setDownload(id, null);
  } catch (err) {
    // Tauri rejects a `Result<_, String>` command with the raw error string.
    const msg = typeof err === "string" ? err : String(err);
    if (isCancel(msg)) setDownload(id, null);
    else setDownload(id, { ...STARTING, phase: "done", error: msg });
  } finally {
    await refreshModelOverview();
  }
}

/** Request an in-flight download to stop (backend discards the partial file). Clears the pill/entry
 *  immediately and swallows the last few in-flight events until the backend actually halts. */
export function cancelModelDownload(id: ModelGroupId): void {
  cancelling.add(id);
  useModelsStore.getState().setDownload(id, null);
  void modelsCancel(id);
  window.setTimeout(() => cancelling.delete(id), 1500);
}

/** Delete a capability's model files (SAM: a specific tier) and refresh the overview. */
export async function removeModel(
  id: ModelGroupId,
  tier?: MaskAiTier,
): Promise<void> {
  await modelsRemove(id, tier);
  await refreshModelOverview();
}

function handleEvent(id: ModelGroupId, p: ModelDownloadEvent): void {
  if (cancelling.has(id)) return; // swallow late events after a user Stop
  const { setDownload } = useModelsStore.getState();
  const now = Date.now();
  const prev = samples[id];
  let bps = prev?.bps ?? 0;
  if (prev && now > prev.t && p.bytesDone >= prev.bytes) {
    const inst = ((p.bytesDone - prev.bytes) / (now - prev.t)) * 1000;
    // EMA smoothing so the readout doesn't jitter chunk-to-chunk.
    bps = prev.bps > 0 ? prev.bps * 0.6 + inst * 0.4 : inst;
  }
  samples[id] = { bytes: p.bytesDone, t: now, bps };
  const remain = Math.max(0, p.bytesTotal - p.bytesDone);
  const etaSec = bps > 0 && remain > 0 ? remain / bps : null;

  setDownload(id, {
    phase: p.phase,
    fileName: p.fileName,
    fileIndex: p.done,
    fileCount: p.total,
    bytesDone: p.bytesDone,
    bytesTotal: p.bytesTotal,
    bytesPerSec: bps,
    etaSec,
    error: null,
  });

  if (p.phase === "done") {
    delete samples[id];
    void refreshModelOverview();
    // Let the bar hit 100% for a beat, then clear the pill/entry.
    window.setTimeout(() => useModelsStore.getState().setDownload(id, null), 600);
  }
}

/**
 * Subscribe to all three `<id>:models` progress streams and keep the models store in sync. Mount
 * EXACTLY ONCE (App.tsx). Captures downloads triggered implicitly (Analyze, first AI-mask click) as
 * well as ones started via {@link startModelDownload}. The modal + global pill read the store and call
 * the exported actions; they must NOT call this hook (that would double-register the listeners).
 */
export function useModelDownloadListeners(): void {
  const bootstrapped = useRef(false);
  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    void refreshModelOverview();

    const unlisteners: UnlistenFn[] = [];
    void (async () => {
      for (const [id, ev] of EVENTS) {
        const un = await listen<ModelDownloadEvent>(ev, (e) =>
          handleEvent(id, e.payload),
        );
        unlisteners.push(un);
      }
    })();

    return () => unlisteners.forEach((fn) => fn());
  }, []);
}
