import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import {
  thumbUrl,
  developGetEdit,
  developRender,
  effectivePreviewEdge,
} from "../../lib/ipc";
import type { DevelopParams, ImageRow, RenderedFrame } from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { freshDefaults } from "../../store/develop";
import {
  deriveViewRect,
  fitViewState,
  zoomAtPoint,
  type ViewState,
  MIN_ZOOM,
  MAX_ZOOM,
} from "../../lib/viewport";
import type { DerivedView } from "../../lib/viewport";

interface LoupeProps {
  image: ImageRow;
}

export default function Loupe({ image }: LoupeProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [containerCss, setContainerCss] = useState({ w: 1, h: 1 });
  const [viewState, setViewState] = useState<ViewState>(fitViewState);
  const viewStateRef = useRef<ViewState>(viewState);
  viewStateRef.current = viewState;

  // The display-sharp preview (crop-applied, so its dims ARE the cropped-frame aspect → no letterbox
  // and a 1:1 match with `develop_render`). Drawn directly at fit/light zoom (no GPU/IPC); only deep
  // zoom past its resolution falls back to the full-res `develop_render`.
  const thumbToken = useAppStore((s) => s.thumbVersions[image.id]);
  const previewImgRef = useRef<HTMLImageElement | null>(null);
  const [previewDims, setPreviewDims] = useState<{
    w: number;
    h: number;
  } | null>(null);

  // Frame dims drive the view rect. Use the preview's real (crop-applied) dims once loaded; before
  // that, the sensor dims are a close-enough fallback for the brief pre-load moment.
  const natural = previewDims ?? {
    w: Math.max(1, image.width ?? 3),
    h: Math.max(1, image.height ?? 2),
  };
  const naturalRef = useRef(natural);
  naturalRef.current = natural;
  const containerCssRef = useRef(containerCss);
  containerCssRef.current = containerCss;

  const dpr = Math.min(
    typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1,
    2,
  );

  const derived = deriveViewRect(
    viewState.zoom,
    viewState.panNorm,
    natural,
    containerCss,
    dpr,
  );

  // ── Container measurement ─────────────────────────────────────────────────
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const measure = () => {
      const r = el.getBoundingClientRect();
      setContainerCss({ w: Math.max(1, r.width), h: Math.max(1, r.height) });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // ── Render state ──────────────────────────────────────────────────────────
  const savedParamsRef = useRef<DevelopParams | null>(null);
  const lastFrameRef = useRef<RenderedFrame | null>(null);
  const rafRef = useRef<number | null>(null);
  const inFlightRef = useRef(false);
  const dirtyRef = useRef(false);
  const scheduleRenderRef = useRef<() => void>(() => {});
  // True while we're in full-res (develop_render) mode — used for zoom hysteresis so a zoom hovering
  // at the threshold doesn't thrash between the preview draw and the GPU decode.
  const decodeModeRef = useRef(false);

  // Draw a view sub-rect of the preview image straight to the canvas (instant; pan/light-zoom never
  // hit the backend). The preview is crop-applied, so view-uv [0,1] maps directly to its pixels.
  const drawPreview = useCallback((d: DerivedView) => {
    const c = canvasRef.current;
    const img = previewImgRef.current;
    if (!c || !img) return;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    const bw = img.naturalWidth;
    const bh = img.naturalHeight;
    ctx.drawImage(
      img,
      d.view.ox * bw,
      d.view.oy * bh,
      d.view.sx * bw,
      d.view.sy * bh,
      0,
      0,
      d.outW,
      d.outH,
    );
  }, []);

  // Single-flight render with a trailing coalesced re-render.
  const scheduleRender = useCallback(() => {
    dirtyRef.current = true;
    if (inFlightRef.current || rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      if (!dirtyRef.current) return;
      dirtyRef.current = false;

      const vs = viewStateRef.current;
      const nat = naturalRef.current;
      const css = containerCssRef.current;
      const dprNow = Math.min(window.devicePixelRatio || 1, 2);
      const d = deriveViewRect(vs.zoom, vs.panNorm, nat, css, dprNow);

      const c = canvasRef.current;
      if (!c) return;
      if (c.width !== d.outW || c.height !== d.outH) {
        c.width = d.outW;
        c.height = d.outH;
      }

      // Decide tier: the preview supplies enough detail when it has ≥1 source pixel per output
      // pixel across the visible window. Hysteresis (0.85×) avoids thrash at the boundary.
      const img = previewImgRef.current;
      const previewPxAcross = img ? d.view.sx * img.naturalWidth : 0;
      let useDecode = decodeModeRef.current
        ? previewPxAcross < d.outW * 0.85
        : previewPxAcross < d.outW;
      if (!img) useDecode = true; // no preview yet → must decode
      decodeModeRef.current = useDecode;

      if (!useDecode) {
        drawPreview(d);
        return;
      }

      // Deep zoom → full-res render. Keep the (slightly soft) preview drawn underneath so there's no
      // blank flash while the decode runs; swap to the sharp frame when it lands.
      inFlightRef.current = true;
      drawPreview(d);
      const p = savedParamsRef.current ?? freshDefaults();
      void developRender(image.id, p, d.view, d.outW, d.outH, -1)
        .then((frame) => {
          if (!frame) return;
          const c2 = canvasRef.current;
          if (!c2) return;
          lastFrameRef.current = frame;
          if (c2.width !== frame.w || c2.height !== frame.h) {
            c2.width = frame.w;
            c2.height = frame.h;
          }
          const ctx = c2.getContext("2d");
          if (ctx)
            ctx.putImageData(new ImageData(frame.data, frame.w, frame.h), 0, 0);
        })
        .catch(() => {
          // Render failed — keep the preview/last frame.
        })
        .finally(() => {
          inFlightRef.current = false;
          if (dirtyRef.current) scheduleRenderRef.current();
        });
    });
  }, [image.id, drawPreview]);
  scheduleRenderRef.current = scheduleRender;

  // ── Per-image load: saved params (for deep-zoom render) + the preview image ───────────────────
  useEffect(() => {
    let cancelled = false;
    previewImgRef.current = null;
    setPreviewDims(null);
    setViewState(fitViewState());
    decodeModeRef.current = false;
    lastFrameRef.current = null;

    savedParamsRef.current = null;
    developGetEdit(image.id)
      .then((p) => {
        if (!cancelled) savedParamsRef.current = p;
      })
      .catch(() => {});

    void effectivePreviewEdge().then((edge) => {
      if (cancelled) return;
      const img = new Image();
      img.onload = () => {
        if (cancelled) return;
        previewImgRef.current = img;
        setPreviewDims({ w: img.naturalWidth, h: img.naturalHeight });
        scheduleRenderRef.current();
      };
      img.src = thumbUrl(
        image.contentHash,
        512,
        image.editedAt,
        thumbToken,
        edge,
      );
    });

    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [image.id, image.contentHash, image.editedAt, thumbToken]);

  // Trigger render on view change (zoom/pan/resize).
  const prevDerived = useRef<string>("");
  const { outW, outH, visCssW, visCssH } = derived;
  const derivedKey = `${outW},${outH},${derived.view.ox.toFixed(6)},${derived.view.oy.toFixed(6)},${derived.view.sx.toFixed(6)},${derived.view.sy.toFixed(6)}`;
  if (prevDerived.current !== derivedKey) {
    prevDerived.current = derivedKey;
    scheduleRender();
  }

  // ── Wheel zoom ────────────────────────────────────────────────────────────
  const onWheel = useCallback((e: WheelEvent) => {
    e.preventDefault();
    const c = canvasRef.current;
    if (!c) return;
    const rect = c.getBoundingClientRect();
    const factor = Math.exp(-e.deltaY * 0.0015);
    const nat = naturalRef.current;
    const css = containerCssRef.current;
    const dprNow = Math.min(window.devicePixelRatio || 1, 2);
    setViewState((prev) => {
      const nextZoom = Math.max(
        MIN_ZOOM,
        Math.min(MAX_ZOOM, prev.zoom * factor),
      );
      return zoomAtPoint(
        prev,
        nextZoom,
        { x: e.clientX - rect.left, y: e.clientY - rect.top },
        nat,
        css,
        dprNow,
      );
    });
  }, []);

  // Attach non-passive wheel listener
  const wheelTargetRef = useRef<HTMLCanvasElement | null>(null);
  if (canvasRef.current !== wheelTargetRef.current) {
    if (wheelTargetRef.current) {
      wheelTargetRef.current.removeEventListener(
        "wheel",
        onWheel as EventListener,
      );
    }
    if (canvasRef.current) {
      canvasRef.current.addEventListener("wheel", onWheel as EventListener, {
        passive: false,
      });
      wheelTargetRef.current = canvasRef.current;
    }
  }

  // ── Drag pan ──────────────────────────────────────────────────────────────
  const onPointerDown = useCallback((e: React.PointerEvent) => {
    if (e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const startY = e.clientY;
    const startState = viewStateRef.current;
    const css = containerCssRef.current;
    const nat = naturalRef.current;
    const dprNow = Math.min(window.devicePixelRatio || 1, 2);
    const startDerived = deriveViewRect(
      startState.zoom,
      startState.panNorm,
      nat,
      css,
      dprNow,
    );
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - startX;
      const dy = ev.clientY - startY;
      const uvDx =
        -(dx / Math.max(1, startDerived.visCssW)) * startDerived.view.sx;
      const uvDy =
        -(dy / Math.max(1, startDerived.visCssH)) * startDerived.view.sy;
      const hsx = startDerived.view.sx / 2;
      const hsy = startDerived.view.sy / 2;
      setViewState({
        zoom: startState.zoom,
        panNorm: {
          x: Math.max(hsx, Math.min(1 - hsx, startState.panNorm.x + uvDx)),
          y: Math.max(hsy, Math.min(1 - hsy, startState.panNorm.y + uvDy)),
        },
      });
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }, []);

  const resetView = useCallback(() => setViewState(fitViewState()), []);

  return (
    <div
      ref={containerRef}
      data-testid="loupe"
      style={{
        width: "100%",
        height: "100%",
        overflow: "hidden",
        background: "var(--color-stage)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        userSelect: "none",
      }}
    >
      <canvas
        ref={canvasRef}
        onPointerDown={onPointerDown}
        onDoubleClick={resetView}
        style={{
          display: "block",
          width: visCssW,
          height: visCssH,
          cursor: "grab",
        }}
      />
    </div>
  );
}
