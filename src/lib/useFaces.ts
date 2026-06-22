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
} from "./ipc";

export type FacesProgress =
  | { kind: "models"; done: number; total: number }
  | { kind: "finding"; done: number; total: number }
  | null;

export interface FacesState {
  status: FacesStatus | null;
  people: PersonRow[];
  progress: FacesProgress;
  /** Bumped on every faces:done — lets consumers re-fetch per-image face data / the grid. */
  doneVersion: number;
}

export interface FacesActions {
  findPeople: (force?: boolean) => Promise<void>;
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
    } catch {
      /* non-fatal */
    }
  }, []);

  const findPeople = useCallback(
    async (force = false) => {
      try {
        const st = await facesStatus();
        setStatus(st);
        if (st.running) return;
        if (!st.modelsReady) {
          setProgress({ kind: "models", done: 0, total: 2 });
          await facesModelsEnsure();
        }
        setStatus((prev) => (prev ? { ...prev, running: true } : prev));
        await facesRun(force);
      } catch {
        /* errors clear via reload below */
      } finally {
        setProgress(null);
        setDoneVersion((v) => v + 1);
        await reload();
      }
    },
    [reload],
  );

  const cancel = useCallback(async () => {
    try {
      await facesCancel();
    } catch {
      /* non-fatal */
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
      unlisteners.push(
        await listen<{ done: number; total: number }>("faces:progress", (ev) =>
          setProgress({
            kind: "finding",
            done: ev.payload.done,
            total: ev.payload.total,
          }),
        ),
      );
      unlisteners.push(
        await listen("faces:done", () => {
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
