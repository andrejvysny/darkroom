import { useCallback, useEffect, useRef, useState } from "react";
import type { DevelopParams } from "./ipc";

export type EditLoad =
  | { kind: "loading" }
  | { kind: "ready"; params: DevelopParams }
  | { kind: "error"; message: string };

/**
 * Per-image saved-edit load state machine. Late results for a superseded image are dropped;
 * `retry` re-runs the load. `stateRef` mirrors `state` for synchronous reads from render
 * callbacks (which must not use a stale closure).
 */
export function useSavedEdit(
  imageId: number | null,
  load: (imageId: number) => Promise<DevelopParams>,
  onReady?: (imageId: number) => void,
): { state: EditLoad; stateRef: React.RefObject<EditLoad>; retry: () => void } {
  const [state, setState] = useState<EditLoad>({ kind: "loading" });
  const stateRef = useRef<EditLoad>({ kind: "loading" });
  const [attempt, setAttempt] = useState(0);
  const onReadyRef = useRef(onReady);
  onReadyRef.current = onReady;

  useEffect(() => {
    let cancelled = false;
    const publish = (next: EditLoad) => {
      if (cancelled) return;
      stateRef.current = next;
      setState(next);
    };
    if (imageId === null) {
      publish({ kind: "loading" });
      return;
    }
    // Reset to loading at the start of each image (and each retry).
    publish({ kind: "loading" });
    load(imageId)
      .then((params) => {
        if (cancelled) return;
        publish({ kind: "ready", params });
        onReadyRef.current?.(imageId);
      })
      .catch((e: unknown) =>
        publish({
          kind: "error",
          message: e instanceof Error ? e.message : String(e),
        }),
      );
    return () => {
      cancelled = true;
    };
  }, [imageId, load, attempt]);

  const retry = useCallback(() => setAttempt((a) => a + 1), []);
  return { state, stateRef, retry };
}
