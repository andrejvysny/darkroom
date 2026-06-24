import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../../store/app";
import { useDevelopStore, freshDefaults } from "../../store/develop";
import {
  developGetEdit,
  developGetHistogram,
  developHistogram,
  developPreviewJpeg,
  developRegenThumb,
  developRender,
  developSession,
  developSetEdit,
  effectivePreviewEdge,
  thumbPrioritize,
  thumbUrl,
  MASK_CAP,
  type BrushStroke,
  type ComponentKind,
  type DevelopParams,
  type HistData,
  type LocalAdjust,
  type Mask,
  type MaskComponent,
  type RenderedFrame,
  type ScalarParamKey,
  type ToneCurveChannel,
  type CurvePoint,
  type HslBand,
  type Crop,
  type CbRgb,
  DEFAULT_CROP,
} from "../../lib/ipc";
import type { DerivedView } from "../../lib/viewport";
import { log } from "../../lib/logger";

// Warm-cache renders are single-digit ms; a short debounce keeps sliders feeling real-time.
const RENDER_DEBOUNCE_MS = 20;
// The whole-crop histogram is a separate (cheap, warm-cache) render; a longer debounce keeps it off
// the hot path during fast slider drags — it only needs to settle, not track every frame.
const HISTOGRAM_DEBOUNCE_MS = 120;

export function useDevelop() {
  const selectedId = useAppStore((s) => s.selectedId);
  const params = useDevelopStore((s) => s.params);
  const previewUrl = useDevelopStore((s) => s.previewUrl);
  const showBefore = useDevelopStore((s) => s.showBefore);
  const setPreviewUrl = useDevelopStore((s) => s.setPreviewUrl);
  const setRendering = useDevelopStore((s) => s.setRendering);
  const setHistogram = useDevelopStore((s) => s.setHistogram);
  const resetParams = useDevelopStore((s) => s.resetParams);
  const maskOverlayVisible = useDevelopStore((s) => s.maskOverlayVisible);
  const selectedMaskIndex = useDevelopStore((s) => s.selectedMaskIndex);

  // Sequence counter for stale-drop: only the latest render wins.
  const renderSeq = useRef(0);
  // Slider interactions since the last persist.
  const touchCount = useRef(0);
  // Preview object URL we own (revoke on image change / unmount).
  const previewObjUrl = useRef<string | null>(null);
  // Tracks the last before/after value so the toggle effect ignores selection changes.
  const prevShowBefore = useRef(showBefore);
  // Last view the canvas passed to us — used to re-render on param/overlay changes.
  const lastDerivedRef = useRef<DerivedView | null>(null);
  // Image id whose whole-crop histogram has been seeded since load (so the first warm render seeds it
  // exactly once — not on every pan/zoom). Reset to null on image change.
  const histogramSeededFor = useRef<number | null>(null);
  // Reactive trigger: bumped on every param/overlay/before-after change so Stage re-renders (paints)
  // the canvas at the current view. (Slider edits don't change the view, so without this they'd only
  // appear on the next zoom/pan.)
  const [renderTick, setRenderTick] = useState(0);

  function setPreview(url: string | null) {
    if (previewObjUrl.current) URL.revokeObjectURL(previewObjUrl.current);
    previewObjUrl.current = url;
    setPreviewUrl(url);
  }

  // Debounced whole-crop histogram refresh — the dedicated full-frame pass (not the viewport buffer),
  // so the panel stays correct while zoomed. Reads showBefore/params at fire time. Skips on the
  // backend if the full-res cache is cold (a warm pass follows the first viewport render).
  const debouncedHistogram = useCallback(
    (() => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const run = (id: number) => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => {
          const st = useDevelopStore.getState();
          const p = st.showBefore ? freshDefaults() : st.params;
          developHistogram(id, p).catch((e) =>
            log.warn("develop", "histogram failed", { imageId: id, ...log.errorSummary(e) }),
          );
        }, HISTOGRAM_DEBOUNCE_MS);
      };
      return Object.assign(run, {
        cancel: () => {
          if (timer !== null) clearTimeout(timer);
          timer = null;
        },
      });
    })(),
    [],
  );

  /**
   * The render function given to Stage/CanvasViewer.
   * Called by the viewport hook on every view change (rAF-throttled).
   * Also called explicitly (via renderTriggerRef bump) when params/overlay change.
   */
  const renderFrame = useCallback(
    async (derived: DerivedView): Promise<RenderedFrame | null> => {
      const id = useAppStore.getState().selectedId;
      if (id === null) return null;

      lastDerivedRef.current = derived;
      const seq = ++renderSeq.current;
      setRendering(true);

      try {
        const st = useDevelopStore.getState();
        const p = st.showBefore ? freshDefaults() : st.params;
        // Crop mode: render full frame so crop overlay maps 1:1
        const rp = st.cropMode ? { ...p, crop: { ...DEFAULT_CROP } } : p;

        const overlayIdx =
          st.maskOverlayVisible && st.selectedMaskIndex !== null
            ? st.selectedMaskIndex
            : -1;

        const frame = await developRender(
          id,
          rp,
          derived.view,
          derived.outW,
          derived.outH,
          overlayIdx,
        );
        if (seq !== renderSeq.current) return null; // superseded
        // The full-res cache is now warm for `id`. Seed the whole-crop histogram once per image open
        // (param edits / before-after toggle trigger their own refresh; pan/zoom must not).
        if (frame && histogramSeededFor.current !== id) {
          histogramSeededFor.current = id;
          debouncedHistogram(id);
        }
        return frame;
      } catch (err) {
        log.warn("develop", "render failed", { imageId: id, ...log.errorSummary(err) });
        return null;
      } finally {
        if (seq === renderSeq.current) setRendering(false);
      }
    },
    [setRendering, debouncedHistogram],
  );

  // Re-render at the current view (used when params/overlay change without a view change). Bumping
  // the tick makes Stage call its (painting) scheduleRender, which re-reads the live params.
  const rerenderCurrent = useCallback(() => {
    setRenderTick((t) => t + 1);
  }, []);

  // Debounced re-render — fast UI feedback on slider drags.
  const debouncedRerender = useCallback(
    (() => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const run = () => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => rerenderCurrent(), RENDER_DEBOUNCE_MS);
      };
      return Object.assign(run, {
        cancel: () => {
          if (timer !== null) clearTimeout(timer);
          timer = null;
        },
      });
    })(),
    [rerenderCurrent],
  );

  // Debounced persist (~500 ms).
  const debouncedPersist = useCallback(
    (() => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const run = (id: number, p: DevelopParams) => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => {
          const tc = touchCount.current;
          touchCount.current = 0;
          developSetEdit(id, p, tc)
            .then(() => developRegenThumb(id))
            .catch((e) => log.warn("develop", "set edit failed", { imageId: id, ...log.errorSummary(e) }));
        }, 500);
      };
      return Object.assign(run, {
        cancel: () => {
          if (timer !== null) clearTimeout(timer);
          timer = null;
        },
      });
    })(),
    [],
  );

  // Apply a new param set: update store, debounce-render, persist.
  const commit = useCallback(
    (id: number, next: DevelopParams) => {
      touchCount.current += 1;
      useDevelopStore.setState({ params: next });
      if (!useDevelopStore.getState().showBefore) {
        debouncedRerender();
        debouncedHistogram(id);
      }
      debouncedPersist(id, next);
    },
    [debouncedRerender, debouncedHistogram, debouncedPersist],
  );

  // Histogram: live updates via event.
  useEffect(() => {
    let active = true;
    const un = listen<HistData>("develop:histogram", (ev) => {
      if (active) setHistogram(ev.payload);
    });
    return () => {
      active = false;
      void un.then((fn) => fn());
    };
  }, [setHistogram]);

  // A preview-source fit render can land before the full-resolution GPU source is ready. The backend
  // warms the full source in the background and emits this event so the canvas swaps to the
  // authoritative full-res render without waiting for another user interaction.
  useEffect(() => {
    let active = true;
    const un = listen<{ imageId: number }>("develop:full-ready", (ev) => {
      if (!active) return;
      const id = useAppStore.getState().selectedId;
      if (id !== ev.payload.imageId) return;
      rerenderCurrent();
      debouncedHistogram(id);
    });
    return () => {
      active = false;
      void un.then((fn) => fn());
    };
  }, [rerenderCurrent, debouncedHistogram]);

  // Load + render on image change.
  useEffect(() => {
    if (selectedId === null) {
      setPreview(null);
      setHistogram(null);
      lastDerivedRef.current = null;
      return;
    }

    const id = selectedId;
    let cancelled = false;
    // New image: the first warm render reseeds the whole-crop histogram.
    histogramSeededFor.current = null;

    useDevelopStore.setState({
      selectedMaskIndex: null,
      pickingColor: false,
      cropMode: false,
      imageAspect: 0,
    });

    // 1. Instant first paint — the display-sharp preview (the SAME look the canvas will show). The
    //    `thumb://` protocol serves the cached preview if ready, else the thumb/placeholder, so one
    //    URL implements "prefer sharp preview, fall back" (decision 3). Falls back to the embedded-JPEG
    //    endpoint if the row isn't in the shared store yet (defensive). Prioritize this image's render.
    const row = useAppStore.getState().libraryImages.find((r) => r.id === id);
    if (row) {
      const v = useAppStore.getState().thumbVersions[id];
      void effectivePreviewEdge().then((edge) => {
        if (cancelled) return;
        setPreview(thumbUrl(row.contentHash, 1024, row.editedAt, v, edge));
      });
    } else {
      developPreviewJpeg(id)
        .then((url) => {
          if (cancelled) {
            URL.revokeObjectURL(url);
            return;
          }
          setPreview(url);
        })
        .catch((e) => log.warn("develop", "preview jpeg failed", { imageId: id, ...log.errorSummary(e) }));
    }
    void thumbPrioritize([id]);

    // 2. Load saved params; renderFrame will be called by the canvas when it mounts.
    (async () => {
      let p: DevelopParams;
      try {
        p = await developGetEdit(id);
      } catch (err) {
        log.warn("develop", "get edit failed", { imageId: id, ...log.errorSummary(err) });
        p = freshDefaults();
      }
      if (cancelled) return;
      useDevelopStore.setState({ params: p });
      // Re-render at current view (canvas may already have a lastDerived from the previous image).
      rerenderCurrent();
      try {
        const h = await developGetHistogram();
        if (!cancelled && h) setHistogram(h);
      } catch (e) {
        log.warn("develop", "get histogram failed", { imageId: id, ...log.errorSummary(e) });
      }
    })();

    return () => {
      cancelled = true;
      setPreview(null);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  // Before/after toggle: re-render only when it actually flips. The histogram follows the displayed
  // image (BEFORE = defaults, AFTER = edited) — debouncedHistogram reads showBefore at fire time.
  useEffect(() => {
    if (selectedId === null) return;
    if (prevShowBefore.current === showBefore) return;
    prevShowBefore.current = showBefore;
    rerenderCurrent();
    debouncedHistogram(selectedId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showBefore]);

  // Overlay toggle: re-render when maskOverlayVisible or selectedMaskIndex changes.
  const prevOverlayKey = useRef(`${maskOverlayVisible}:${selectedMaskIndex}`);
  useEffect(() => {
    const key = `${maskOverlayVisible}:${selectedMaskIndex}`;
    if (prevOverlayKey.current === key) return;
    prevOverlayKey.current = key;
    if (selectedId !== null) rerenderCurrent();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [maskOverlayVisible, selectedMaskIndex]);

  // Cleanup on unmount.
  useEffect(() => {
    return () => {
      if (previewObjUrl.current) {
        URL.revokeObjectURL(previewObjUrl.current);
        previewObjUrl.current = null;
      }
    };
  }, []);

  // On leaving an image (navigate away / close Develop), enqueue it so the background queue fills its
  // display-sharp preview tier. Edited previews are generated lazily here — NOT on every slider settle
  // (a full-size render each settle would jank editing); `develop_regen_thumb` keeps the small edited
  // thumb current per settle, and this catches the larger preview once editing stops.
  useEffect(() => {
    if (selectedId === null) return;
    const id = selectedId;
    return () => {
      void thumbPrioritize([id]);
    };
  }, [selectedId]);

  // Mark a Develop session open for the lifetime of this hook so the background thumbnail worker
  // parks between jobs and interactive renders win the GPU.
  useEffect(() => {
    void developSession(true);
    return () => {
      void developSession(false);
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

  const onCropChange = useCallback(
    (patch: Partial<Crop>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const next = { ...cur, crop: { ...cur.crop, ...patch } };
      if (useDevelopStore.getState().cropMode) {
        touchCount.current += 1;
        useDevelopStore.setState({ params: next });
        debouncedPersist(selectedId, next);
      } else {
        commit(selectedId, next);
      }
    },
    [selectedId, commit, debouncedPersist],
  );

  const onColorBalanceChange = useCallback(
    (patch: Partial<CbRgb>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      commit(selectedId, { ...cur, cbRgb: { ...cur.cbRgb, ...patch } });
    },
    [selectedId, commit],
  );

  // ── Mask operations ───────────────────────────────────────────────────────

  const addMask = useCallback(
    (mask: Mask) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      if (cur.masks.length >= MASK_CAP) return;
      const masks = [...cur.masks, mask];
      commit(selectedId, { ...cur, masks });
      useDevelopStore.setState({
        selectedMaskIndex: masks.length - 1,
        selectedComponentIndex: 0,
      });
    },
    [selectedId, commit],
  );

  const updateMask = useCallback(
    (index: number, patch: Partial<Mask>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const masks = cur.masks.map((m, i) =>
        i === index ? { ...m, ...patch } : m,
      );
      commit(selectedId, { ...cur, masks });
    },
    [selectedId, commit],
  );

  const updateMaskAdjust = useCallback(
    (index: number, patch: Partial<LocalAdjust>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const m = cur.masks[index];
      if (!m) return;
      const masks = cur.masks.map((mm, i) =>
        i === index ? { ...mm, adjust: { ...mm.adjust, ...patch } } : mm,
      );
      commit(selectedId, { ...cur, masks });
    },
    [selectedId, commit],
  );

  const updateMaskComponentKind = useCallback(
    (index: number, compIndex: number, kind: ComponentKind) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const m = cur.masks[index];
      if (!m || !m.components[compIndex]) return;
      const components = m.components.map((c, i) =>
        i === compIndex ? { ...c, kind } : c,
      );
      const masks = cur.masks.map((mm, i) =>
        i === index ? { ...mm, components } : mm,
      );
      commit(selectedId, { ...cur, masks });
    },
    [selectedId, commit],
  );

  const addComponent = useCallback(
    (index: number, component: MaskComponent) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const m = cur.masks[index];
      if (!m) return;
      const components = [...m.components, component];
      const masks = cur.masks.map((mm, i) =>
        i === index ? { ...mm, components } : mm,
      );
      commit(selectedId, { ...cur, masks });
      useDevelopStore.setState({
        selectedComponentIndex: components.length - 1,
      });
    },
    [selectedId, commit],
  );

  const updateComponent = useCallback(
    (index: number, compIndex: number, patch: Partial<MaskComponent>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const m = cur.masks[index];
      if (!m || !m.components[compIndex]) return;
      const components = m.components.map((c, i) =>
        i === compIndex ? { ...c, ...patch } : c,
      );
      const masks = cur.masks.map((mm, i) =>
        i === index ? { ...mm, components } : mm,
      );
      commit(selectedId, { ...cur, masks });
    },
    [selectedId, commit],
  );

  const deleteComponent = useCallback(
    (index: number, compIndex: number) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const m = cur.masks[index];
      if (!m || m.components.length <= 1) return;
      const components = m.components.filter((_, i) => i !== compIndex);
      const masks = cur.masks.map((mm, i) =>
        i === index ? { ...mm, components } : mm,
      );
      commit(selectedId, { ...cur, masks });
      const sel = useDevelopStore.getState().selectedComponentIndex;
      useDevelopStore.setState({
        selectedComponentIndex: Math.min(sel, components.length - 1),
      });
    },
    [selectedId, commit],
  );

  const appendStroke = useCallback(
    (index: number, stroke: BrushStroke) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const m = cur.masks[index];
      if (!m) return;
      const ci = m.components.findIndex((c) => c.kind.type === "brush");
      if (ci < 0) return;
      const comp = m.components[ci];
      if (comp.kind.type !== "brush") return;
      const strokes = [...comp.kind.strokes, stroke];
      const components = m.components.map((c, i) =>
        i === ci ? { ...c, kind: { type: "brush" as const, strokes } } : c,
      );
      const masks = cur.masks.map((mm, i) =>
        i === index ? { ...mm, components } : mm,
      );
      commit(selectedId, { ...cur, masks });
    },
    [selectedId, commit],
  );

  const deleteMask = useCallback(
    (index: number) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const masks = cur.masks.filter((_, i) => i !== index);
      commit(selectedId, { ...cur, masks });
      const sel = useDevelopStore.getState().selectedMaskIndex;
      const next =
        sel === null || masks.length === 0
          ? null
          : Math.min(sel, masks.length - 1);
      useDevelopStore.setState({ selectedMaskIndex: next });
    },
    [selectedId, commit],
  );

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
    debouncedPersist.cancel();
    debouncedRerender.cancel();
    touchCount.current = 0;
    resetParams();
    const p = freshDefaults();
    rerenderCurrent();
    developSetEdit(selectedId, p, undefined, true)
      .then(() => developRegenThumb(selectedId))
      .catch((e) =>
        log.warn("develop", "reset edit failed", {
          imageId: selectedId,
          ...log.errorSummary(e),
        }),
      );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    selectedId,
    resetParams,
    debouncedPersist,
    debouncedRerender,
    rerenderCurrent,
  ]);

  // Re-render when the crop tool opens/closes.
  const cropMode = useDevelopStore((s) => s.cropMode);
  useEffect(() => {
    const id = useAppStore.getState().selectedId;
    if (id !== null && !useDevelopStore.getState().showBefore) {
      rerenderCurrent();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cropMode]);

  return {
    params,
    previewUrl,
    renderFrame,
    renderTick,
    onParamChange,
    onCurveChange,
    onHslChange,
    onCropChange,
    onColorBalanceChange,
    resetKeys,
    reset,
    addMask,
    updateMask,
    updateMaskAdjust,
    updateMaskComponentKind,
    addComponent,
    updateComponent,
    deleteComponent,
    appendStroke,
    deleteMask,
  };
}
