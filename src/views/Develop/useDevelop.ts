import { useEffect, useRef, useCallback } from "react";
import { useAppStore } from "../../store/app";
import { useDevelopStore, freshDefaults } from "../../store/develop";
import {
  developGetEdit,
  developRender,
  developSetEdit,
  type DevelopParams,
  type ScalarParamKey,
  type ToneCurveChannel,
  type CurvePoint,
  type HslBand,
} from "../../lib/ipc";

export function useDevelop() {
  const selectedId = useAppStore((s) => s.selectedId);
  const { params, resetParams, imageUrl, setImageUrl, setRendering } =
    useDevelopStore();
  const showBefore = useDevelopStore((s) => s.showBefore);

  // Sequence counter for stale-drop: only the latest render wins.
  const renderSeq = useRef(0);
  // Hold the current object URL so we can revoke it.
  const currentUrl = useRef<string | null>(null);

  function applyUrl(url: string) {
    if (currentUrl.current) URL.revokeObjectURL(currentUrl.current);
    currentUrl.current = url;
    setImageUrl(url);
  }

  async function render(id: number, p: DevelopParams) {
    const seq = ++renderSeq.current;
    setRendering(true);
    try {
      const url = await developRender(id, p);
      if (seq !== renderSeq.current) {
        // Stale — discard immediately.
        URL.revokeObjectURL(url);
        return;
      }
      applyUrl(url);
    } catch (err) {
      console.error("develop_render failed", err);
    } finally {
      if (seq === renderSeq.current) setRendering(false);
    }
  }

  // Debounced render (~60 ms) — fast UI feedback.
  const debouncedRender = useCallback(
    (() => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      return (id: number, p: DevelopParams) => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => render(id, p), 60);
      };
    })(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );

  // Debounced persist (~500 ms).
  const debouncedPersist = useCallback(
    (() => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      return (id: number, p: DevelopParams) => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => {
          developSetEdit(id, p).catch((e) =>
            console.error("develop_set_edit failed", e),
          );
        }, 500);
      };
    })(),
    [],
  );

  // Apply a new param set: update the store, render (unless showing "before"), and persist.
  const commit = useCallback(
    (id: number, next: DevelopParams) => {
      useDevelopStore.setState({ params: next });
      if (!useDevelopStore.getState().showBefore) debouncedRender(id, next);
      debouncedPersist(id, next);
    },
    [debouncedRender, debouncedPersist],
  );

  // Load + render on image change.
  useEffect(() => {
    if (selectedId === null) {
      // Revoke stale URL when no image selected.
      if (currentUrl.current) {
        URL.revokeObjectURL(currentUrl.current);
        currentUrl.current = null;
      }
      setImageUrl(null);
      return;
    }

    let cancelled = false;

    (async () => {
      try {
        const loaded = await developGetEdit(selectedId);
        if (cancelled) return;
        useDevelopStore.setState({ params: loaded });
        await render(selectedId, loaded);
      } catch (err) {
        console.error("develop_get_edit failed", err);
        // Fallback: render with defaults.
        if (!cancelled) {
          const d = freshDefaults();
          useDevelopStore.setState({ params: d });
          await render(selectedId, d);
        }
      }
    })();

    return () => {
      cancelled = true;
      // Revoke URL on image change.
      if (currentUrl.current) {
        URL.revokeObjectURL(currentUrl.current);
        currentUrl.current = null;
        setImageUrl(null);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  // Before/after: re-render with DEFAULT params when toggled on, current params when off.
  useEffect(() => {
    if (selectedId === null) return;
    const p = showBefore ? freshDefaults() : useDevelopStore.getState().params;
    render(selectedId, p);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showBefore, selectedId]);

  // Cleanup on unmount.
  useEffect(() => {
    return () => {
      if (currentUrl.current) {
        URL.revokeObjectURL(currentUrl.current);
        currentUrl.current = null;
      }
    };
  }, []);

  const onParamChange = useCallback(
    (key: ScalarParamKey, value: number) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      commit(selectedId, { ...cur, [key]: value });
    },
    [selectedId, commit],
  );

  const onCurveChange = useCallback(
    (channel: ToneCurveChannel, points: CurvePoint[]) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      commit(selectedId, {
        ...cur,
        toneCurve: { ...cur.toneCurve, [channel]: points },
      });
    },
    [selectedId, commit],
  );

  const onHslChange = useCallback(
    (index: number, patch: Partial<HslBand>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const hsl = cur.hsl.map((b, i) => (i === index ? { ...b, ...patch } : b));
      commit(selectedId, { ...cur, hsl });
    },
    [selectedId, commit],
  );

  // Reset a single module's scalar keys to their defaults in one render/persist.
  const resetKeys = useCallback(
    (keys: ScalarParamKey[]) => {
      if (selectedId === null) return;
      const d = freshDefaults();
      const cur = useDevelopStore.getState().params;
      const next = { ...cur };
      for (const k of keys) (next[k] as number) = d[k] as number;
      commit(selectedId, next);
    },
    [selectedId, commit],
  );

  const reset = useCallback(() => {
    if (selectedId === null) return;
    resetParams();
    const p = freshDefaults();
    render(selectedId, p);
    developSetEdit(selectedId, p).catch((e) =>
      console.error("develop_set_edit failed", e),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId, resetParams]);

  return {
    params,
    imageUrl,
    onParamChange,
    onCurveChange,
    onHslChange,
    resetKeys,
    reset,
  };
}
