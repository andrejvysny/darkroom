import { useCallback, useLayoutEffect, useRef, useState } from "react";
import MaskOverlay from "./MaskOverlay";
import CropOverlay from "./CropOverlay";
import { useDevelopStore } from "../../store/develop";
import { useAppStore } from "../../store/app";
import {
  type BrushStroke,
  type ComponentKind,
  type Crop,
  type Mask,
} from "../../lib/ipc";
import {
  deriveViewRect,
  fitViewState,
  zoomAtPoint,
  zoom1to1,
  type DerivedView,
  type ViewState,
  MIN_ZOOM,
  MAX_ZOOM,
} from "../../lib/viewport";
import type { RenderedFrame } from "../../lib/ipc";

const PAD = 40;

interface StageProps {
  /** True while the original (unedited) render is shown. */
  showBefore: boolean;
  rendering: boolean;
  masks: Mask[];
  crop: Crop;
  /** Natural sensor dimensions of the current image (drives readout + viewport math). */
  natural: { w: number; h: number };
  onCropChange: (patch: Partial<Crop>) => void;
  onChangeMaskKind: (
    index: number,
    compIndex: number,
    kind: ComponentKind,
  ) => void;
  onCommitStroke: (index: number, stroke: BrushStroke) => void;
  /** Render function: receives current derived view, returns frame or null. */
  renderFn: (derived: DerivedView) => Promise<RenderedFrame | null>;
  /** Bumped by useDevelop on every param/overlay/before-after change to force a canvas re-render. */
  renderTick: number;
  /** Embedded preview <img> for instant first paint; painted to canvas synchronously. */
  previewImg?: HTMLImageElement | null;
}

export default function Stage({
  showBefore,
  rendering,
  masks,
  crop,
  natural,
  onCropChange,
  onChangeMaskKind,
  onCommitStroke,
  renderFn,
  renderTick,
  previewImg,
}: StageProps) {
  const selectedMaskIndex = useDevelopStore((s) => s.selectedMaskIndex);
  const selectedComponentIndex = useDevelopStore(
    (s) => s.selectedComponentIndex,
  );
  const maskOverlayVisible = useDevelopStore((s) => s.maskOverlayVisible);
  const cropMode = useDevelopStore((s) => s.cropMode);
  const brush = useDevelopStore((s) => s.brush);
  const selectedId = useAppStore((s) => s.selectedId);

  const containerRef = useRef<HTMLElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [containerCss, setContainerCss] = useState({ w: 1, h: 1 });

  // Measure the available stage area (minus padding)
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const measure = () =>
      setContainerCss({
        w: Math.max(1, el.clientWidth - PAD * 2),
        h: Math.max(1, el.clientHeight - PAD * 2),
      });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // View state: zoom + pan
  const [viewState, setViewState] = useState<ViewState>(fitViewState);
  const viewStateRef = useRef<ViewState>(viewState);
  viewStateRef.current = viewState;

  // Reset zoom/pan when the image changes (not on every render)
  const prevSelectedId = useRef<number | null>(null);
  if (prevSelectedId.current !== selectedId) {
    prevSelectedId.current = selectedId;
    // Reset synchronously to avoid a flash of the old pan/zoom
    if (
      viewState.zoom !== 1 ||
      viewState.panNorm.x !== 0.5 ||
      viewState.panNorm.y !== 0.5
    ) {
      const fresh = fitViewState();
      setViewState(fresh);
      viewStateRef.current = fresh;
    }
  }

  const dpr = Math.min(
    typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1,
    2,
  );

  // The rendered frame's pixel dims. In crop mode the backend renders the FULL frame (so the crop
  // overlay maps 1:1); otherwise it renders the cropped frame, whose aspect = sensor × crop extent.
  // Using sensor dims here when a crop is applied would distort the view rect + misalign overlays.
  const effNatural = cropMode
    ? natural
    : {
        w: Math.max(1, natural.w * crop.hw * 2),
        h: Math.max(1, natural.h * crop.hh * 2),
      };

  // Crop mode locks to fit (render full frame so overlay maps 1:1)
  const effectiveZoomState = cropMode ? fitViewState() : viewState;

  const derived = deriveViewRect(
    effectiveZoomState.zoom,
    effectiveZoomState.panNorm,
    effNatural,
    containerCss,
    dpr,
  );

  // Last successfully painted frame — double-buffering
  const lastFrameRef = useRef<RenderedFrame | null>(null);
  // rAF scheduling — single-flight with a trailing coalesced render.
  const rafRef = useRef<number | null>(null);
  const inFlightRef = useRef(false);
  const dirtyRef = useRef(false);
  const scheduleRenderRef = useRef<() => void>(() => {});

  const naturalRef = useRef(effNatural);
  naturalRef.current = effNatural;
  const containerCssRef = useRef(containerCss);
  containerCssRef.current = containerCss;
  const renderFnRef = useRef(renderFn);
  renderFnRef.current = renderFn;

  // Size canvas backing store to visCss × dpr
  const { outW, outH, visCssW, visCssH } = derived;
  const canvas = canvasRef.current;
  if (canvas && (canvas.width !== outW || canvas.height !== outH)) {
    canvas.width = outW;
    canvas.height = outH;
    // Only repaint the cached frame if it matches the new size (avoids a wrong-scale flash); on a
    // genuine resize the dims differ, so the next render (scheduled below) repaints within ~5 ms.
    const last = lastFrameRef.current;
    if (last && last.w === outW && last.h === outH) {
      const ctx = canvas.getContext("2d");
      if (ctx) ctx.putImageData(new ImageData(last.data, last.w, last.h), 0, 0);
    }
  }

  // Schedule a render. At most ONE render is in flight; a burst of view changes coalesces into one
  // rAF, and a change that arrives DURING an in-flight render queues exactly one trailing render —
  // so a fast drag/wheel never piles up unbounded server round-trips (it converges to the latest).
  const scheduleRender = useCallback(() => {
    dirtyRef.current = true;
    if (inFlightRef.current || rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      if (!dirtyRef.current) return;
      dirtyRef.current = false;
      inFlightRef.current = true;
      const vs = useDevelopStore.getState().cropMode
        ? fitViewState()
        : viewStateRef.current;
      const nat = naturalRef.current;
      const css = containerCssRef.current;
      const dprNow = Math.min(window.devicePixelRatio || 1, 2);
      const d = deriveViewRect(vs.zoom, vs.panNorm, nat, css, dprNow);
      void renderFnRef
        .current(d)
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
        .finally(() => {
          inFlightRef.current = false;
          if (dirtyRef.current) scheduleRenderRef.current();
        });
    });
  }, []);
  scheduleRenderRef.current = scheduleRender;

  // Trigger render when view changes
  const prevDerived = useRef<string>("");
  const derivedKey = `${outW},${outH},${derived.view.ox.toFixed(6)},${derived.view.oy.toFixed(6)},${derived.view.sx.toFixed(6)},${derived.view.sy.toFixed(6)}`;
  if (prevDerived.current !== derivedKey) {
    prevDerived.current = derivedKey;
    scheduleRender();
  }

  // Trigger render when params/overlay/before-after change (no view change) — useDevelop bumps this.
  const prevTick = useRef(renderTick);
  if (prevTick.current !== renderTick) {
    prevTick.current = renderTick;
    scheduleRender();
  }

  // Paint preview image when it arrives
  const prevPreviewImg = useRef<HTMLImageElement | null>(null);
  if (previewImg && previewImg !== prevPreviewImg.current) {
    prevPreviewImg.current = previewImg;
    const c = canvasRef.current;
    if (c) {
      const ctx = c.getContext("2d");
      if (ctx) ctx.drawImage(previewImg, 0, 0, c.width, c.height);
    }
  }

  // Wheel zoom
  const onWheel = useCallback(
    (e: WheelEvent) => {
      if (cropMode) return;
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
    },
    [cropMode],
  );

  // Attach wheel listener (must be non-passive to preventDefault)
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

  // Drag pan
  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (cropMode || e.button !== 0) return;
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
        const newCx = startState.panNorm.x + uvDx;
        const newCy = startState.panNorm.y + uvDy;
        const hsx = startDerived.view.sx / 2;
        const hsy = startDerived.view.sy / 2;
        setViewState({
          zoom: startState.zoom,
          panNorm: {
            x: Math.max(hsx, Math.min(1 - hsx, newCx)),
            y: Math.max(hsy, Math.min(1 - hsy, newCy)),
          },
        });
      };
      const up = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
      };
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    },
    [cropMode],
  );

  const resetView = useCallback(() => {
    setViewState(fitViewState());
  }, []);

  const selectedMask =
    selectedMaskIndex !== null ? masks[selectedMaskIndex] : undefined;
  const showOverlay =
    maskOverlayVisible && !showBefore && selectedMask !== undefined;

  // Zoom readout: show as % relative to 1:1 device-pixel zoom
  const oneToOne = zoom1to1(natural, containerCss, dpr);
  const zoomPct = Math.round((viewState.zoom / oneToOne) * 100);

  return (
    <section
      ref={containerRef}
      style={{
        flex: "1 1 auto",
        background: "var(--color-stage-dev)",
        position: "relative",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: PAD,
        minWidth: 0,
        minHeight: 0,
        overflow: "hidden",
      }}
    >
      {/* Canvas wrapper — shadow matches old <img> wrapper */}
      <div
        style={{
          position: "relative",
          width: visCssW,
          height: visCssH,
          flexShrink: 0,
          boxShadow:
            "0 10px 50px rgba(0,0,0,.55), 0 0 0 1px rgba(255,255,255,.06)",
          borderRadius: 3,
          overflow: "hidden",
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
            borderRadius: 3,
            userSelect: "none",
            cursor: cropMode ? "default" : "grab",
          }}
        />

        {/* Overlays sit above canvas, sized to the visible image rect */}
        {showOverlay && (
          <div
            style={{ position: "absolute", inset: 0, pointerEvents: "none" }}
          >
            <MaskOverlay
              width={visCssW}
              height={visCssH}
              viewRect={derived.view}
              mask={selectedMask}
              compIndex={Math.min(
                selectedComponentIndex,
                selectedMask.components.length - 1,
              )}
              onChangeKind={(kind) =>
                onChangeMaskKind(
                  selectedMaskIndex!,
                  Math.min(
                    selectedComponentIndex,
                    selectedMask.components.length - 1,
                  ),
                  kind,
                )
              }
              brush={brush}
              onCommitStroke={(stroke) =>
                onCommitStroke(selectedMaskIndex!, stroke)
              }
            />
          </div>
        )}
        {cropMode && !showBefore && (
          <div
            style={{ position: "absolute", inset: 0, pointerEvents: "none" }}
          >
            <CropOverlay
              width={visCssW}
              height={visCssH}
              crop={crop}
              onChange={onCropChange}
            />
          </div>
        )}
      </div>

      {/* BEFORE badge */}
      {showBefore && (
        <div
          style={{
            position: "absolute",
            top: 14,
            left: 16,
            padding: "3px 8px",
            borderRadius: "var(--radius-sm)",
            background: "var(--color-accent)",
            color: "#fff",
            fontSize: 10.5,
            fontWeight: 600,
            letterSpacing: 0.5,
            fontFamily: "var(--font-mono)",
          }}
        >
          BEFORE
        </div>
      )}

      {/* Rendering indicator dot */}
      {rendering && (
        <div
          style={{
            position: "absolute",
            top: 14,
            right: 14,
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: "var(--color-accent)",
            opacity: 0.8,
          }}
        />
      )}

      {/* Status bar */}
      <div
        style={{
          position: "absolute",
          left: 16,
          bottom: 14,
          display: "flex",
          alignItems: "center",
          gap: 14,
          fontSize: 11.5,
          color: "var(--color-t3)",
          fontFamily: "var(--font-mono)",
        }}
      >
        <span
          onClick={resetView}
          style={{ cursor: "pointer" }}
          title="Reset zoom (double-click stage)"
        >
          {zoomPct}%
        </span>
        <Dot />
        <span>
          {natural.w} × {natural.h}
        </span>
        <Dot />
        <span>sRGB</span>
      </div>
    </section>
  );
}

function Dot() {
  return (
    <span
      style={{
        width: 4,
        height: 4,
        borderRadius: "50%",
        background: "var(--color-t3)",
        display: "block",
      }}
    />
  );
}
