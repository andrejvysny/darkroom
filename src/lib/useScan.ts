import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  scanCancel,
  scanRun,
  scanRunning,
  type ScanScope,
  type ScanSelection,
  type StageId,
} from "./ipc";
import { log } from "./logger";
import { useAppStore } from "../store/app";

/**
 * Which phase the running scan is in. The backend emits these on `analysis:progress`; `finalize` is
 * the post-Stop face-clustering pass, which is why Stop can still show progress after being pressed.
 */
export type ScanPhase = "models" | "detect" | "caption" | "finalize" | "panorama";

export type ScanProgress = {
  phase: ScanPhase;
  done: number;
  total: number;
} | null;

const PHASE_LABEL: Record<ScanPhase, string> = {
  models: "Downloading models",
  detect: "Analyzing",
  caption: "Captioning",
  finalize: "Finalising faces",
  panorama: "Finding panoramas",
};

/** Human progress line for the grid pill. */
export function scanProgressLabel(p: ScanProgress, scopeLabel: string | null): string {
  if (!p) return "";
  const where = p.phase === "panorama" || !scopeLabel ? "" : ` in ${scopeLabel}`;
  // `finalize` reports 0/0 until the first poll of the unlocked pass — don't show a bogus fraction.
  if (p.total <= 0) return `${PHASE_LABEL[p.phase]}…`;
  return `${PHASE_LABEL[p.phase]} ${p.done} / ${p.total}${where}…`;
}

export interface ScanState {
  running: boolean;
  progress: ScanProgress;
  /** Bumped whenever a scan finishes, so consumers can re-fetch derived data. */
  doneVersion: number;
}

export interface ScanActions {
  run: (selection: ScanSelection, scopeLabel: string | null) => Promise<void>;
  cancel: () => Promise<void>;
}

/**
 * Drives the unified scan: one run action, one cancel, one progress stream.
 *
 * Listeners are module-scoped singletons (as in `usePanoDetect`) because both the sidebar button and
 * the grid pill call this hook — per-instance listeners would double every toast and state write.
 */
let bootstrapped = false;
const listeners = new Set<() => void>();
let shared: { running: boolean; progress: ScanProgress; doneVersion: number } = {
  running: false,
  progress: null,
  doneVersion: 0,
};

function publish(patch: Partial<typeof shared>): void {
  shared = { ...shared, ...patch };
  listeners.forEach((fn) => fn());
}

function bootstrap(): void {
  if (bootstrapped) return;
  bootstrapped = true;

  // Re-attach to a scan that is still running from before a reload.
  void scanRunning()
    .then((r) => {
      if (r) publish({ running: true });
    })
    .catch(() => {});

  void listen<{ phase: string; done: number; total: number }>(
    "analysis:progress",
    (ev) => {
      const phase = ev.payload.phase as ScanPhase;
      publish({
        running: true,
        progress: { phase, done: ev.payload.done, total: ev.payload.total },
      });
    },
  );
  void listen<{ done: number; total: number }>("analysis:models", (ev) => {
    publish({
      running: true,
      progress: { phase: "models", done: ev.payload.done, total: ev.payload.total },
    });
  });
  void listen<{ phase: string; done: number; total: number }>(
    "pano_detect:progress",
    (ev) => {
      publish({
        running: true,
        progress: {
          phase: "panorama",
          done: ev.payload.done,
          total: ev.payload.total,
        },
      });
    },
  );
  // `scan:done` is the authoritative end of the whole job. The per-phase `analysis:done` /
  // `pano_detect:done` events still fire between phases, so they must NOT clear `running` here or
  // the pill would blink out mid-scan.
  void listen<{ cancelled: boolean }>("scan:done", () => {
    publish({ running: false, progress: null, doneVersion: shared.doneVersion + 1 });
  });
}

export function useScan(): ScanState & ScanActions {
  bootstrap();
  const [, force] = useState(0);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    const rerender = () => {
      if (mounted.current) force((v) => v + 1);
    };
    listeners.add(rerender);
    return () => {
      mounted.current = false;
      listeners.delete(rerender);
    };
  }, []);

  const run = useCallback(
    async (selection: ScanSelection, scopeLabel: string | null) => {
      const { setScanScopeLabel, setToast } = useAppStore.getState();
      // Optimistic: the button must dim immediately, before the first progress event.
      publish({ running: true, progress: null });
      setScanScopeLabel(selection.scope ? scopeLabel : null);
      try {
        const r = await scanRun(selection);
        if (r.cancelled) {
          setToast(
            r.analyzed > 0
              ? `Scan stopped — kept results for ${r.analyzed.toLocaleString()} photo${r.analyzed === 1 ? "" : "s"}`
              : "Scan stopped",
          );
        } else if (r.panoramasFound > 0) {
          setToast(
            `Scan complete — ${r.panoramasFound} panorama suggestion${r.panoramasFound === 1 ? "" : "s"}`,
          );
        }
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err);
        setToast(`Scan failed: ${msg}`);
        log.warn("scan", "run failed", log.errorSummary(err));
      } finally {
        publish({ running: false, progress: null, doneVersion: shared.doneVersion + 1 });
        setScanScopeLabel(null);
      }
    },
    [],
  );

  const cancel = useCallback(async () => {
    try {
      await scanCancel();
    } catch (err) {
      log.debug("scan", "cancel failed", log.errorSummary(err));
    }
  }, []);

  return { ...shared, run, cancel };
}

/** Convenience for callers that only need to launch a scan for a scope. */
export function selectionFor(
  stages: StageId[],
  scope: ScanScope | null,
  force: boolean,
): ScanSelection {
  return { stages, scope, force };
}
