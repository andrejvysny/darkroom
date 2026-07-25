import { useState, useEffect, useCallback, useRef } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  facesStatus,
  facesModelsEnsure,
  facesRun,
  facesCancel,
  peopleList,
  type FacesStatus,
  type PersonRow,
  type ScanScope,
} from "./ipc";
import { log } from "./logger";
import { useAppStore } from "../store/app";

export type FacesProgress =
  | { kind: "models"; done: number; total: number }
  | { kind: "finding"; done: number; total: number }
  | null;

export interface FacesState {
  status: FacesStatus | null;
  people: PersonRow[];
  progress: FacesProgress;
  /** Bumped on every scan completion (analysis:done) — lets consumers re-fetch per-image face data. */
  doneVersion: number;
}

export interface FacesActions {
  /** `scope` narrows detection+embedding to one container; clustering stays library-wide.
   *  `scopeLabel` is that container's display name, shown in the progress pill. */
  findPeople: (
    force?: boolean,
    scope?: ScanScope | null,
    scopeLabel?: string | null,
  ) => Promise<void>;
  cancel: () => Promise<void>;
  reload: () => Promise<void>;
}

/** Drives the People sidebar: status, the clustered people list, and the "Find People" pass. Mirrors
 *  `useAnalysis` (downloads models on first run; progress events update the bar). */
export function useFaces(): FacesState & FacesActions {
  const [status, setStatus] = useState<FacesStatus | null>(null);
  const [people, setPeople] = useState<PersonRow[]>([]);
  const [progress, setProgress] = useState<FacesProgress>(null);
  const [doneVersion, setDoneVersion] = useState(0);
  const bootstrapped = useRef(false);

  const reload = useCallback(async () => {
    try {
      const [st, ppl] = await Promise.all([facesStatus(), peopleList(false)]);
      setStatus(st);
      setPeople(ppl);
    } catch (err) {
      log.debug("faces", "reload failed", log.errorSummary(err));
    }
  }, []);

  const findPeople = useCallback(
    async (
      force = false,
      scope: ScanScope | null = null,
      scopeLabel: string | null = null,
    ) => {
      try {
        const st = await facesStatus();
        setStatus(st);
        if (st.running) return;
        // Surfaced by the progress pill; cleared in `finally` so a cancel/throw can't strand it.
        useAppStore.getState().setScanScopeLabel(scope ? scopeLabel : null);
        if (!st.modelsReady) {
          setProgress({ kind: "models", done: 0, total: 2 });
          await facesModelsEnsure();
        }
        setStatus((prev) => (prev ? { ...prev, running: true } : prev));
        await facesRun(force, scope);
      } catch (err) {
        log.warn("faces", "find people failed", { force, ...log.errorSummary(err) });
      } finally {
        setProgress(null);
        setDoneVersion((v) => v + 1);
        useAppStore.getState().setScanScopeLabel(null);
        await reload();
      }
    },
    [reload],
  );

  const cancel = useCallback(async () => {
    try {
      await facesCancel();
    } catch (err) {
      log.debug("faces", "cancel failed", log.errorSummary(err));
    }
  }, []);

  useEffect(() => {
    if (bootstrapped.current) return;
    bootstrapped.current = true;
    void reload();

    const unlisteners: UnlistenFn[] = [];
    async function setup() {
      unlisteners.push(
        await listen<{ done: number; total: number }>("faces:models", (ev) =>
          setProgress({
            kind: "models",
            done: ev.payload.done,
            total: ev.payload.total,
          }),
        ),
      );
      // Faces run inside the unified scan, so progress/completion ride the `analysis:*` stream (only
      // the model download keeps its own `faces:models` event). Skip the caption phase — face work is
      // already done by then.
      unlisteners.push(
        await listen<{ phase?: string; done: number; total: number }>(
          "analysis:progress",
          (ev) => {
            if (ev.payload.phase === "caption") return;
            setProgress({
              kind: "finding",
              done: ev.payload.done,
              total: ev.payload.total,
            });
          },
        ),
      );
      unlisteners.push(
        await listen("analysis:done", () => {
          setProgress(null);
          setDoneVersion((v) => v + 1);
          void reload();
        }),
      );
    }
    void setup();
    return () => unlisteners.forEach((fn) => fn());
  }, [reload]);

  return { status, people, progress, doneVersion, findPeople, cancel, reload };
}
