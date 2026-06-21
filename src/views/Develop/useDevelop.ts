import { useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAppStore } from "../../store/app";
import { useDevelopStore, freshDefaults } from "../../store/develop";
import {
  developGetEdit,
  developGetHistogram,
  developPreviewJpeg,
  developRegenThumb,
  developRender,
  developSetEdit,
  MASK_CAP,
  type BrushStroke,
  type ComponentKind,
  type DevelopParams,
  type HistData,
  type LocalAdjust,
  type Mask,
  type MaskComponent,
  type ScalarParamKey,
  type ToneCurveChannel,
  type CurvePoint,
  type HslBand,
  type Crop,
  DEFAULT_CROP,
} from "../../lib/ipc";

// Warm-cache renders are single-digit ms; a short debounce keeps sliders feeling real-time while
// still coalescing rapid drags.
const RENDER_DEBOUNCE_MS = 20;

export function useDevelop() {
  const selectedId = useAppStore((s) => s.selectedId);
  // Subscribe only to values the UI reads; setters are stable (no re-render).
  const params = useDevelopStore((s) => s.params);
  const imageUrl = useDevelopStore((s) => s.imageUrl);
  const previewUrl = useDevelopStore((s) => s.previewUrl);
  const showBefore = useDevelopStore((s) => s.showBefore);
  const setImageUrl = useDevelopStore((s) => s.setImageUrl);
  const setPreviewUrl = useDevelopStore((s) => s.setPreviewUrl);
  const setRendering = useDevelopStore((s) => s.setRendering);
  const setHistogram = useDevelopStore((s) => s.setHistogram);
  const resetParams = useDevelopStore((s) => s.resetParams);

  // Sequence counter for stale-drop: only the latest render wins.
  const renderSeq = useRef(0);
  // Slider interactions since the last persist — a deliberation weight for the behavioral log.
  const touchCount = useRef(0);
  // Object URLs we own, so we can revoke them.
  const currentUrl = useRef<string | null>(null);
  const previewObjUrl = useRef<string | null>(null);
  // Tracks the last before/after value so the toggle effect ignores selection changes.
  const prevShowBefore = useRef(showBefore);
  // Tracks the last full-res value so its toggle effect ignores selection changes.
  const fullRes = useDevelopStore((s) => s.fullRes);
  const prevFullRes = useRef(fullRes);

  function applyUrl(url: string) {
    if (currentUrl.current) URL.revokeObjectURL(currentUrl.current);
    currentUrl.current = url;
    setImageUrl(url);
  }

  function setPreview(url: string | null) {
    if (previewObjUrl.current) URL.revokeObjectURL(previewObjUrl.current);
    previewObjUrl.current = url;
    setPreviewUrl(url);
  }

  // Returns true if this render won (was applied), false if stale/superseded/failed.
  async function render(id: number, p: DevelopParams): Promise<boolean> {
    const seq = ++renderSeq.current;
    setRendering(true);
    try {
      // While the crop tool is active, render the FULL un-cropped/un-rotated image so the crop
      // overlay maps 1:1 to image coordinates; the applied crop renders when the tool is closed.
      const st = useDevelopStore.getState();
      const rp = st.cropMode ? { ...p, crop: { ...DEFAULT_CROP } } : p;
      // Full-res tier when zoomed in past fit (set by the Stage); preview tier otherwise.
      const url = await developRender(id, rp, st.fullRes);
      if (seq !== renderSeq.current) {
        if (url) URL.revokeObjectURL(url); // a newer render superseded this one
        return false;
      }
      if (url) applyUrl(url); // null → backend skipped a superseded request
      return url !== null;
    } catch (err) {
      console.error("develop_render failed", err);
      return false;
    } finally {
      if (seq === renderSeq.current) setRendering(false);
    }
  }

  // Debounced render — fast UI feedback.
  const debouncedRender = useCallback(
    (() => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const run = (id: number, p: DevelopParams) => {
        if (timer !== null) clearTimeout(timer);
        timer = setTimeout(() => render(id, p), RENDER_DEBOUNCE_MS);
      };
      return Object.assign(run, {
        cancel: () => {
          if (timer !== null) clearTimeout(timer);
          timer = null;
        },
      });
    })(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
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
          // Persist, then regenerate the edited thumbnail so the filmstrip/grid/loupe update live
          // (the regen command emits `develop:edit-changed`).
          developSetEdit(id, p, tc)
            .then(() => developRegenThumb(id))
            .catch((e) => console.error("develop_set_edit failed", e));
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

  // Apply a new param set: update the store, render (unless showing "before"), and persist.
  const commit = useCallback(
    (id: number, next: DevelopParams) => {
      touchCount.current += 1;
      useDevelopStore.setState({ params: next });
      if (!useDevelopStore.getState().showBefore) debouncedRender(id, next);
      debouncedPersist(id, next);
    },
    [debouncedRender, debouncedPersist],
  );

  // Histogram: a fire-and-forget event keeps it live during edits. Registered once.
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

  // Load + render on image change: instant embedded preview, then the processed render.
  useEffect(() => {
    if (selectedId === null) {
      setPreview(null);
      if (currentUrl.current) {
        URL.revokeObjectURL(currentUrl.current);
        currentUrl.current = null;
      }
      setImageUrl(null);
      setHistogram(null);
      return;
    }

    const id = selectedId;
    let cancelled = false;
    // New image: drop any mask selection / armed eyedropper / crop tool from the previous image, and
    // invalidate the cached image aspect (the Stage re-sets it on load) so crop presets never use a
    // stale (different-image) aspect. Leaving cropMode on would render the new image uncropped.
    useDevelopStore.setState({
      selectedMaskIndex: null,
      pickingColor: false,
      cropMode: false,
      imageAspect: 0,
    });

    // 1. Instant first paint — embedded camera JPEG (demosaic-free), in parallel with the render.
    developPreviewJpeg(id)
      .then((url) => {
        if (cancelled) {
          URL.revokeObjectURL(url);
          return;
        }
        setPreview(url);
      })
      .catch((e) => console.error("develop_preview_jpeg failed", e));

    // 2. Saved params → processed render → guaranteed histogram (pull covers a missed event).
    (async () => {
      let p: DevelopParams;
      try {
        p = await developGetEdit(id);
      } catch (err) {
        console.error("develop_get_edit failed", err);
        p = freshDefaults();
      }
      if (cancelled) return;
      useDevelopStore.setState({ params: p });
      const renderParams = useDevelopStore.getState().showBefore
        ? freshDefaults()
        : p;
      const won = await render(id, renderParams);
      if (cancelled || !won) return;
      try {
        const h = await developGetHistogram();
        if (!cancelled && h) setHistogram(h);
      } catch (e) {
        console.error("develop_get_histogram failed", e);
      }
    })();

    return () => {
      cancelled = true;
      if (currentUrl.current) {
        URL.revokeObjectURL(currentUrl.current);
        currentUrl.current = null;
        setImageUrl(null);
      }
      setPreview(null);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId]);

  // Before/after: re-render only when the toggle actually flips (not on image change — the load
  // effect already renders the correct before/after variant for a new image).
  useEffect(() => {
    if (selectedId === null) return;
    if (prevShowBefore.current === showBefore) return;
    prevShowBefore.current = showBefore;
    const p = showBefore ? freshDefaults() : useDevelopStore.getState().params;
    render(selectedId, p);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showBefore]);

  // Re-render at the new resolution tier when the stage zoom crosses the fit threshold.
  useEffect(() => {
    if (selectedId === null) return;
    if (prevFullRes.current === fullRes) return;
    prevFullRes.current = fullRes;
    const p = useDevelopStore.getState().showBefore
      ? freshDefaults()
      : useDevelopStore.getState().params;
    render(selectedId, p);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fullRes]);

  // Cleanup on unmount.
  useEffect(() => {
    return () => {
      if (currentUrl.current) {
        URL.revokeObjectURL(currentUrl.current);
        currentUrl.current = null;
      }
      if (previewObjUrl.current) {
        URL.revokeObjectURL(previewObjUrl.current);
        previewObjUrl.current = null;
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

  const onCropChange = useCallback(
    (patch: Partial<Crop>) => {
      if (selectedId === null) return;
      const cur = useDevelopStore.getState().params;
      const next = { ...cur, crop: { ...cur.crop, ...patch } };
      // While the crop tool is active the preview shows the full (identity-crop) image, so a crop
      // change does NOT alter the rendered pixels — update the store + persist, but skip the
      // redundant GPU re-render (the overlay is pure DOM). The applied crop renders on tool close
      // via the cropMode effect below.
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

  // ── Mask operations ───────────────────────────────────────────────────────
  // All build a new params.masks and route through commit() (same render/persist path as sliders).

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

  // Replace the kind (geometry) of one component of a mask — used by the overlay drag handlers.
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

  // Append a new component to a mask (for Add/Subtract/Intersect combining).
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

  // Patch a component's top-level fields (op / invert / feather).
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
      if (!m || m.components.length <= 1) return; // keep at least one
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

  // Append a brush stroke to a mask's first brush component (used by the painting overlay).
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
      // Keep the selection valid after removal.
      const next =
        sel === null || masks.length === 0
          ? null
          : Math.min(sel, masks.length - 1);
      useDevelopStore.setState({ selectedMaskIndex: next });
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
    // Cancel any pending debounced persist/render armed by a just-moved slider. Without this, a
    // persist scheduled by the last commit() fires ~500 ms later and writes the PRE-reset params
    // back over the defaults we're about to save — silently reverting the reset on disk.
    debouncedPersist.cancel();
    debouncedRender.cancel();
    touchCount.current = 0;
    resetParams();
    const p = freshDefaults();
    render(selectedId, p);
    // force=true: Reset deliberately discards the stored edit, even if the prior blob was unreadable.
    developSetEdit(selectedId, p, undefined, true)
      .then(() => developRegenThumb(selectedId))
      .catch((e) => console.error("develop_set_edit failed", e));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedId, resetParams, debouncedPersist, debouncedRender]);

  // Re-render when the crop tool opens/closes (full image while editing → applied crop on close).
  const cropMode = useDevelopStore((s) => s.cropMode);
  useEffect(() => {
    const id = useAppStore.getState().selectedId;
    if (id !== null && !useDevelopStore.getState().showBefore) {
      debouncedRender(id, useDevelopStore.getState().params);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cropMode]);

  return {
    params,
    imageUrl,
    previewUrl,
    onParamChange,
    onCurveChange,
    onHslChange,
    onCropChange,
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
