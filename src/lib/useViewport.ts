import {
  useCallback,
  useLayoutEffect,
  useRef,
  useState,
  type Dispatch,
  type RefObject,
  type SetStateAction,
} from "react";
import {
  deriveViewRect,
  fitViewState,
  zoomAtPoint,
  type DerivedView,
  type ViewState,
  MIN_ZOOM,
  MAX_ZOOM,
} from "./viewport";

const MAX_DPR = 2;

function clampDpr(): number {
  return Math.min(
    typeof window !== "undefined" ? window.devicePixelRatio || 1 : 1,
    MAX_DPR,
  );
}

const IDENTITY = (v: ViewState): ViewState => v;

function depsChanged(prev: unknown[] | undefined, next: unknown[]): boolean {
  if (!prev || prev.length !== next.length) return true;
  for (let i = 0; i < next.length; i++) {
    if (!Object.is(prev[i], next[i])) return true;
  }
  return false;
}

export interface UseViewportOptions {
  /**
   * The RENDERED FRAME's pixel dims (crop-adjusted by the caller — Stage passes its `effNatural`,
   * Loupe its preview/sensor dims). The hook never knows about crop.
   */
  natural: { w: number; h: number };

  /**
   * Paint callback, run inside the single-flight rAF with the FIRE-TIME derived view. It OWNS all
   * canvas sizing + drawing (the hook intentionally does NOT pre-size the canvas, so an async painter
   * — Stage — has no transparent-resize flash; a sync painter — Loupe's preview tier — sizes then
   * draws immediately). The hook re-schedules a trailing render if dirtied during the await.
   */
  render: (derived: DerivedView, canvas: HTMLCanvasElement) => Promise<void>;

  /** Reset view to fit (synchronously, guarded) when this changes — Stage: selectedId, Loupe: image.id. */
  resetKey: unknown;

  /** Padding subtracted on each side when measuring the container (Stage 40, Loupe 0). */
  pad?: number;

  /** Measurement strategy: 'client' = clientWidth/Height (Stage), 'boundingRect' (Loupe, default). */
  measure?: "client" | "boundingRect";

  /** When false, wheel-zoom AND drag-pan are disabled (Stage passes `!cropMode`). Default true. */
  interactive?: boolean;

  /**
   * Transform the live view state before deriving — used to inject Stage's crop-mode fit-lock without
   * leaking crop concepts into the hook. The hook owns the live state and passes it in; called both
   * render-phase (for overlays / `derivedKey`) and at fire time inside the rAF. Default: identity.
   */
  transformViewState?: (live: ViewState) => ViewState;

  /** Extra reschedule triggers (shallow-compared): when any changes, schedule a render. Stage: [renderTick]. */
  renderDeps?: unknown[];
}

export interface UseViewportApi<C extends HTMLElement = HTMLElement> {
  containerRef: RefObject<C | null>;
  canvasRef: RefObject<HTMLCanvasElement | null>;
  /** Render-phase derived view (after `transformViewState`) — for overlays / canvas wrapper sizing. */
  derived: DerivedView;
  /** Container CSS size after pad subtraction (for the zoom-readout `zoom1to1`). */
  containerCss: { w: number; h: number };
  /** Effective dpr (min(devicePixelRatio, 2)). */
  dpr: number;
  viewState: ViewState;
  setViewState: Dispatch<SetStateAction<ViewState>>;
  resetView: () => void;
  /** Imperative single-flight scheduler (e.g. Loupe's image `onload` fires it). */
  scheduleRender: () => void;
  onPointerDown: (e: React.PointerEvent) => void;
}

/**
 * Shared canvas-viewport machinery for Develop's Stage and Library's Loupe: container measurement,
 * zoom/pan state, the single-flight rAF render scheduler (re-derives at fire time for freshness),
 * wheel-zoom, drag-pan, fit-reset. The caller supplies the `render` body (Stage's `renderFn`→paint,
 * Loupe's tiered preview/decode) and any crop fit-lock via `transformViewState`.
 */
export function useViewport<C extends HTMLElement = HTMLElement>(
  opts: UseViewportOptions,
): UseViewportApi<C> {
  const {
    natural,
    render,
    resetKey,
    pad = 0,
    measure = "boundingRect",
    interactive = true,
    transformViewState,
    renderDeps,
  } = opts;

  const containerRef = useRef<C | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [containerCss, setContainerCss] = useState({ w: 1, h: 1 });

  // Measure the available area (minus padding). [] deps — strategy/pad read through refs so a change
  // never re-subscribes the observer.
  const padRef = useRef(pad);
  padRef.current = pad;
  const measureRef = useRef(measure);
  measureRef.current = measure;
  useLayoutEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const doMeasure = () => {
      const p = padRef.current;
      if (measureRef.current === "client") {
        setContainerCss({
          w: Math.max(1, el.clientWidth - p * 2),
          h: Math.max(1, el.clientHeight - p * 2),
        });
      } else {
        const r = el.getBoundingClientRect();
        setContainerCss({
          w: Math.max(1, r.width - p * 2),
          h: Math.max(1, r.height - p * 2),
        });
      }
    };
    doMeasure();
    const ro = new ResizeObserver(doMeasure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const [viewState, setViewState] = useState<ViewState>(fitViewState);
  const viewStateRef = useRef<ViewState>(viewState);
  viewStateRef.current = viewState;

  // Synchronous, guarded fit reset on resetKey change (avoids a flash of the old pan/zoom).
  const prevResetKey = useRef<unknown>(resetKey);
  if (prevResetKey.current !== resetKey) {
    prevResetKey.current = resetKey;
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

  // Refs the [] -deps scheduler / handlers read at fire time (freshness without re-subscribing).
  const naturalRef = useRef(natural);
  naturalRef.current = natural;
  const containerCssRef = useRef(containerCss);
  containerCssRef.current = containerCss;
  const renderRef = useRef(render);
  renderRef.current = render;
  const interactiveRef = useRef(interactive);
  interactiveRef.current = interactive;
  const transformRef = useRef<(live: ViewState) => ViewState>(
    transformViewState ?? IDENTITY,
  );
  transformRef.current = transformViewState ?? IDENTITY;

  const dpr = clampDpr();

  // Render-phase derived (after transform) — drives overlays/JSX sizing + the derivedKey trigger.
  const effState = transformRef.current(viewState);
  const derived = deriveViewRect(
    effState.zoom,
    effState.panNorm,
    natural,
    containerCss,
    dpr,
  );

  // Single-flight rAF scheduler with a trailing coalesced render. Re-derives the view at fire time
  // (fresh pan during a drag) from the live ref + transform.
  const rafRef = useRef<number | null>(null);
  const inFlightRef = useRef(false);
  const dirtyRef = useRef(false);
  const scheduleRenderRef = useRef<() => void>(() => {});
  const scheduleRender = useCallback(() => {
    dirtyRef.current = true;
    if (inFlightRef.current || rafRef.current !== null) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = null;
      if (!dirtyRef.current) return;
      const c = canvasRef.current;
      if (!c) {
        // Canvas not mounted yet — keep dirty and retry next frame (don't drop the render).
        scheduleRenderRef.current();
        return;
      }
      dirtyRef.current = false;
      inFlightRef.current = true;
      const vs = transformRef.current(viewStateRef.current);
      const nat = naturalRef.current;
      const css = containerCssRef.current;
      const dprNow = clampDpr();
      const d = deriveViewRect(vs.zoom, vs.panNorm, nat, css, dprNow);
      void renderRef.current(d, c).finally(() => {
        inFlightRef.current = false;
        if (dirtyRef.current) scheduleRenderRef.current();
      });
    });
  }, []);
  scheduleRenderRef.current = scheduleRender;

  // Schedule on view change.
  const prevDerivedKey = useRef<string>("");
  const derivedKey = `${derived.outW},${derived.outH},${derived.view.ox.toFixed(6)},${derived.view.oy.toFixed(6)},${derived.view.sx.toFixed(6)},${derived.view.sy.toFixed(6)}`;
  if (prevDerivedKey.current !== derivedKey) {
    prevDerivedKey.current = derivedKey;
    scheduleRender();
  }

  // Schedule on external triggers (param/overlay/before-after change → Stage's renderTick).
  const prevRenderDeps = useRef<unknown[] | undefined>(renderDeps);
  if (renderDeps && depsChanged(prevRenderDeps.current, renderDeps)) {
    prevRenderDeps.current = renderDeps;
    scheduleRender();
  }

  // Wheel zoom (anchored at cursor). Reads `interactive` through a ref so the stable listener (below)
  // always sees the live value; the listener is (re)attached on canvas-element identity only.
  const onWheel = useCallback((e: WheelEvent) => {
    if (!interactiveRef.current) return;
    e.preventDefault();
    const c = canvasRef.current;
    if (!c) return;
    const rect = c.getBoundingClientRect();
    const factor = Math.exp(-e.deltaY * 0.0015);
    const nat = naturalRef.current;
    const css = containerCssRef.current;
    const dprNow = clampDpr();
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

  // Attach the wheel listener non-passively (so preventDefault works); re-attach on canvas swap only.
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

  // Drag pan (clamped so the window stays within [0,1]).
  const onPointerDown = useCallback((e: React.PointerEvent) => {
    if (!interactiveRef.current || e.button !== 0) return;
    e.preventDefault();
    const startX = e.clientX;
    const startY = e.clientY;
    const startState = viewStateRef.current;
    const css = containerCssRef.current;
    const nat = naturalRef.current;
    const dprNow = clampDpr();
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

  return {
    containerRef,
    canvasRef,
    derived,
    containerCss,
    dpr,
    viewState,
    setViewState,
    resetView,
    scheduleRender,
    onPointerDown,
  };
}
