import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import { thumbUrl, developGetEdit, developRender } from "../../lib/ipc";
import type { ImageRow, RenderedFrame } from "../../lib/ipc";
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

  // The rendered frame's pixel dims = sensor dims × the saved crop extent (set once params load).
  // Using sensor dims when a crop is saved would distort the view rect for cropped images.
  const [cropExtent, setCropExtent] = useState({ hw: 0.5, hh: 0.5 });
  const natural = {
    w: Math.max(1, (image.width ?? 3) * cropExtent.hw * 2),
    h: Math.max(1, (image.height ?? 2) * cropExtent.hh * 2),
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

  // ── Saved params (read-once per image for the render call) ────────────────
  const savedParamsRef = useRef<Awaited<
    ReturnType<typeof developGetEdit>
  > | null>(null);

  useEffect(() => {
    savedParamsRef.current = null;
    setCropExtent({ hw: 0.5, hh: 0.5 });
    setViewState(fitViewState());
    developGetEdit(image.id)
      .then((p) => {
        savedParamsRef.current = p;
        if (p?.crop) setCropExtent({ hw: p.crop.hw, hh: p.crop.hh });
        scheduleRender();
      })
      .catch(() => {
        savedParamsRef.current = null;
        scheduleRender();
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [image.id]);

  // ── Double-buffering ──────────────────────────────────────────────────────
  const lastFrameRef = useRef<RenderedFrame | null>(null);
  const rafRef = useRef<number | null>(null);
  const inFlightRef = useRef(false);
  const dirtyRef = useRef(false);
  const scheduleRenderRef = useRef<() => void>(() => {});

  // Size canvas backing store
  const canvas = canvasRef.current;
  const { outW, outH, visCssW, visCssH } = derived;
  if (canvas && (canvas.width !== outW || canvas.height !== outH)) {
    canvas.width = outW;
    canvas.height = outH;
    const last = lastFrameRef.current;
    if (last && last.w === outW && last.h === outH) {
      const ctx = canvas.getContext("2d");
      if (ctx) ctx.putImageData(new ImageData(last.data, last.w, last.h), 0, 0);
    }
  }

  // Single-flight render with a trailing coalesced re-render (see Stage/useViewport).
  const scheduleRender = useCallback(() => {
    dirtyRef.current = true;
    if (inFlightRef.current || rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      if (!dirtyRef.current) return;
      dirtyRef.current = false;
      inFlightRef.current = true;
      const vs = viewStateRef.current;
      const nat = naturalRef.current;
      const css = containerCssRef.current;
      const dprNow = Math.min(window.devicePixelRatio || 1, 2);
      const d: DerivedView = deriveViewRect(
        vs.zoom,
        vs.panNorm,
        nat,
        css,
        dprNow,
      );
      const p = savedParamsRef.current ?? freshDefaults();
      void developRender(image.id, p, d.view, d.outW, d.outH, -1)
        .then((frame) => {
          if (!frame) return;
          const c = canvasRef.current;
          if (!c) return;
          lastFrameRef.current = frame;
          if (c.width !== frame.w || c.height !== frame.h) {
            c.width = frame.w;
            c.height = frame.h;
          }
          const ctx = c.getContext("2d");
          if (!ctx) return;
          ctx.putImageData(new ImageData(frame.data, frame.w, frame.h), 0, 0);
        })
        .catch(() => {
          // Render failed — keep the last frame.
        })
        .finally(() => {
          inFlightRef.current = false;
          if (dirtyRef.current) scheduleRenderRef.current();
        });
    });
  }, [image.id]);
  scheduleRenderRef.current = scheduleRender;

  // Trigger render on view change
  const prevDerived = useRef<string>("");
  const derivedKey = `${outW},${outH},${derived.view.ox.toFixed(6)},${derived.view.oy.toFixed(6)},${derived.view.sx.toFixed(6)},${derived.view.sy.toFixed(6)}`;
  if (prevDerived.current !== derivedKey) {
    prevDerived.current = derivedKey;
    scheduleRender();
  }

  // ── Instant first paint: thumb while the render arrives ───────────────────
  const thumbSrc = thumbUrl(image.contentHash, 512, image.editedAt);
  useEffect(() => {
    const c = canvasRef.current;
    if (!c) return;
    const img = new Image();
    img.onload = () => {
      const ctx = c.getContext("2d");
      if (ctx) ctx.drawImage(img, 0, 0, c.width, c.height);
    };
    img.src = thumbSrc;
  }, [image.id, thumbSrc]);

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
