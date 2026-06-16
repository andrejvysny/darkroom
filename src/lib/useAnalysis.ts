import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  analysisStatus,
  analysisModelsEnsure,
  analysisRun,
  analysisFacets,
  type AnalysisStatus,
  type FacetRow,
} from "./ipc";

export type AnalysisProgress =
  | { kind: "models"; done: number; total: number }
  | { kind: "analyzing"; done: number; total: number }
  | null;

export interface AnalysisState {
  status: AnalysisStatus | null;
  facets: FacetRow[];
  progress: AnalysisProgress;
  /** Bumped on every analysis:done — lets consumers re-fetch per-image AI data. */
  doneVersion: number;
}

export interface AnalysisActions {
  triggerAnalysis: (force?: boolean) => Promise<void>;
  reloadFacets: () => Promise<void>;
}

export function useAnalysis(): AnalysisState & AnalysisActions {
  const [status, setStatus] = useState<AnalysisStatus | null>(null);
  const [facets, setFacets] = useState<FacetRow[]>([]);
  const [progress, setProgress] = useState<AnalysisProgress>(null);
  const [doneVersion, setDoneVersion] = useState(0);

  // Guard against double-run in React 19 StrictMode
  const bootstrappedRef = useRef(false);

  const reloadFacets = useCallback(async () => {
    try {
      setFacets(await analysisFacets());
    } catch {
      /* non-fatal */
    }
  }, []);

  const reloadStatus = useCallback(async () => {
    try {
      setStatus(await analysisStatus());
    } catch {
      /* non-fatal */
    }
  }, []);

  const triggerAnalysis = useCallback(
    async (force = false) => {
      try {
        const st = await analysisStatus();
        setStatus(st);
        if (st.running) return; // already in flight

        if (!st.modelsReady) {
          // Download models first; progress events drive the UI
          await analysisModelsEnsure();
        }

        setStatus((prev) =>
          prev ? { ...prev, running: true } : { ...st, running: true },
        );
        await analysisRun(force);
      } catch {
        /* errors surface via progress state clearing */
      } finally {
        setProgress(null);
        setDoneVersion((v) => v + 1);
        await Promise.all([reloadStatus(), reloadFacets()]);
      }
    },
    [reloadStatus, reloadFacets],
  );

  // Mount: fetch initial status + facets, register event listeners
  useEffect(() => {
    if (bootstrappedRef.current) return;
    bootstrappedRef.current = true;

    void Promise.all([reloadStatus(), reloadFacets()]);

    const unlisteners: UnlistenFn[] = [];

    async function setup() {
      const unModels = await listen<{ done: number; total: number }>(
        "analysis:models",
        (ev) =>
          setProgress({
            kind: "models",
            done: ev.payload.done,
            total: ev.payload.total,
          }),
      );
      unlisteners.push(unModels);

      const unProgress = await listen<{ done: number; total: number }>(
        "analysis:progress",
        (ev) =>
          setProgress({
            kind: "analyzing",
            done: ev.payload.done,
            total: ev.payload.total,
          }),
      );
      unlisteners.push(unProgress);

      const unDone = await listen<{ analyzed: number; failed: number }>(
        "analysis:done",
        () => {
          setProgress(null);
          setDoneVersion((v) => v + 1);
          void Promise.all([reloadStatus(), reloadFacets()]);
        },
      );
      unlisteners.push(unDone);
    }

    void setup();

    return () => {
      unlisteners.forEach((fn) => fn());
    };
  }, [reloadStatus, reloadFacets]);

  return {
    status,
    facets,
    progress,
    doneVersion,
    triggerAnalysis,
    reloadFacets,
  };
}
