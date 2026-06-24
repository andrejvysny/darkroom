import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  analysisStatus,
  analysisModelsEnsure,
  analysisRun,
  analysisCancel,
  analysisFacets,
  type AnalysisStatus,
  type FacetRow,
} from "./ipc";
import { log } from "./logger";

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
  cancelAnalysis: () => Promise<void>;
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
    } catch (err) {
      log.debug("analysis", "reload facets failed", log.errorSummary(err));
    }
  }, []);

  // Throttled live refresh during a run: reload facet counts and bump doneVersion so consumers
  // (sidebar counts, per-image AI panel, filtered grid) pick up incrementally-committed results.
  const liveTimer = useRef<number | null>(null);
  const liveLast = useRef(0);
  const scheduleLiveRefresh = useCallback(() => {
    const fire = () => {
      liveLast.current = Date.now();
      void reloadFacets();
      setDoneVersion((v) => v + 1);
    };
    const since = Date.now() - liveLast.current;
    if (since >= 800) {
      fire();
    } else if (liveTimer.current === null) {
      liveTimer.current = window.setTimeout(() => {
        liveTimer.current = null;
        fire();
      }, 800 - since);
    }
  }, [reloadFacets]);

  const reloadStatus = useCallback(async () => {
    try {
      setStatus(await analysisStatus());
    } catch (err) {
      log.debug("analysis", "reload status failed", log.errorSummary(err));
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
      } catch (err) {
        log.warn("analysis", "run failed", { force, ...log.errorSummary(err) });
      } finally {
        setProgress(null);
        setDoneVersion((v) => v + 1);
        await Promise.all([reloadStatus(), reloadFacets()]);
      }
    },
    [reloadStatus, reloadFacets],
  );

  const cancelAnalysis = useCallback(async () => {
    try {
      await analysisCancel();
    } catch (err) {
      log.debug("analysis", "cancel failed", log.errorSummary(err));
    }
  }, []);

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
        (ev) => {
          setProgress({
            kind: "analyzing",
            done: ev.payload.done,
            total: ev.payload.total,
          });
          scheduleLiveRefresh();
        },
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
      if (liveTimer.current !== null) window.clearTimeout(liveTimer.current);
    };
  }, [reloadStatus, reloadFacets, scheduleLiveRefresh]);

  return {
    status,
    facets,
    progress,
    doneVersion,
    triggerAnalysis,
    cancelAnalysis,
    reloadFacets,
  };
}
