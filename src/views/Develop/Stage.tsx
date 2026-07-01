import { useRef, useState } from "react";
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
  fitViewState,
  zoom1to1,
  type DerivedView,
  type ViewRect,
} from "../../lib/viewport";
import { useViewport } from "../../lib/useViewport";
import { paintFrame } from "../../lib/canvasPaint";
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
  /** False while natural dims are unknown (renders suppressed); flipping true re-schedules. */
  renderGate?: boolean;
  /** Embedded preview <img> for instant first paint; painted to canvas synchronously. */
  previewImg?: HTMLImageElement | null;
  /** Set when the last render threw (auto-retry exhausted); shows the retry overlay. */
  renderError?: string | null;
  /** Manual retry for the error overlay. */
  onRetryRender?: () => void;
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
  renderGate = true,
  previewImg,
  renderError,
  onRetryRender,
}: StageProps) {
  const selectedMaskIndex = useDevelopStore((s) => s.selectedMaskIndex);
  const selectedComponentIndex = useDevelopStore(
    (s) => s.selectedComponentIndex,
  );
  const maskOverlayVisible = useDevelopStore((s) => s.maskOverlayVisible);
  const cropMode = useDevelopStore((s) => s.cropMode);
  const brush = useDevelopStore((s) => s.brush);
  const selectedId = useAppStore((s) => s.selectedId);

  // The rendered frame's pixel dims. In crop mode the backend renders the FULL frame (so the crop
  // overlay maps 1:1); otherwise it renders the cropped frame, whose aspect = sensor × crop extent.
  // Using sensor dims here when a crop is applied would distort the view rect + misalign overlays.
  const effNatural = cropMode
    ? natural
    : {
        w: Math.max(1, natural.w * crop.hw * 2),
        h: Math.max(1, natural.h * crop.hh * 2),
      };

  // Last successfully painted frame (preview-gate + image-change reset).
  const lastFrameRef = useRef<RenderedFrame | null>(null);
  // CSS size + view rect OF THE PAINTED FRAME. The canvas box is sized from this — never from the
  // live derived view — so the displayed pixels and their box always agree in aspect. Sizing the
  // box from the live view while the async GPU frame lags is exactly what stretched the image
  // along one axis (and wiped it to blank) during wheel zoom.
  const [painted, setPainted] = useState<{
    cssW: number;
    cssH: number;
    view: ViewRect;
  } | null>(null);

  // New image: forget the previous image's frame + painted box so the preview gate reopens and no
  // stale-size box flashes.
  const prevSelectedId = useRef(selectedId);
  if (prevSelectedId.current !== selectedId) {
    prevSelectedId.current = selectedId;
    lastFrameRef.current = null;
    setPainted(null);
  }

  const {
    containerRef,
    canvasRef,
    derived,
    containerCss,
    dpr,
    viewState,
    resetView,
    onPointerDown,
  } = useViewport<HTMLElement>({
    natural: effNatural,
    pad: PAD,
    measure: "client",
    // Crop mode locks to fit (render full frame so the overlay maps 1:1) + disables wheel/pan.
    interactive: !cropMode,
    transformViewState: (live) =>
      useDevelopStore.getState().cropMode ? fitViewState() : live,
    render: async (d, canvas) => {
      const frame = await renderFn(d);
      if (frame) {
        lastFrameRef.current = frame;
        // Size + paint the backing store, THEN publish the matching CSS box + view — one atomic
        // "painted state". Nothing else ever resizes or clears the canvas.
        paintFrame(canvas, frame);
        setPainted((p) =>
          p &&
          p.cssW === d.visCssW &&
          p.cssH === d.visCssH &&
          p.view.ox === d.view.ox &&
          p.view.oy === d.view.oy &&
          p.view.sx === d.view.sx &&
          p.view.sy === d.view.sy
            ? p
            : { cssW: d.visCssW, cssH: d.visCssH, view: d.view },
        );
      }
    },
    resetKey: selectedId,
    renderDeps: [renderTick, renderGate],
  });

  const { outW, outH, visCssW, visCssH } = derived;

  // The canvas box follows the PAINTED frame; before the first paint it uses the live derived size
  // (empty canvas, correct placeholder box).
  const boxW = painted?.cssW ?? visCssW;
  const boxH = painted?.cssH ?? visCssH;
  const paintedView = painted?.view ?? derived.view;

  // Paint preview image when it arrives (instant first paint before the GPU render lands).
  // Gated: only before any GPU frame exists AND only at the fit view with no crop — the preview is
  // the WHOLE (uncropped) image, so stretch-blitting it into a zoomed/panned sub-window or a
  // cropped frame distorts. A late-arriving preview must never overwrite a GPU frame.
  const prevPreviewImg = useRef<HTMLImageElement | null>(null);
  if (previewImg && previewImg !== prevPreviewImg.current) {
    prevPreviewImg.current = previewImg;
    const v = derived.view;
    const fitView =
      Math.abs(v.ox) < 1e-6 &&
      Math.abs(v.oy) < 1e-6 &&
      Math.abs(v.sx - 1) < 1e-6 &&
      Math.abs(v.sy - 1) < 1e-6;
    const uncropped = crop.hw === 0.5 && crop.hh === 0.5;
    if (lastFrameRef.current === null && fitView && uncropped) {
      const c = canvasRef.current;
      if (c) {
        // The preview is the first content: size the backing store itself (atomically with the
        // draw) and publish the matching box.
        if (c.width !== outW || c.height !== outH) {
          c.width = outW;
          c.height = outH;
        }
        const ctx = c.getContext("2d");
        if (ctx) ctx.drawImage(previewImg, 0, 0, c.width, c.height);
        setPainted({ cssW: visCssW, cssH: visCssH, view: v });
      }
    }
  }

  const selectedMask =
    selectedMaskIndex !== null ? masks[selectedMaskIndex] : undefined;
  const showOverlay =
    maskOverlayVisible && !showBefore && selectedMask !== undefined;

  // Zoom readout: show as % relative to 1:1 device-pixel zoom (uses SENSOR dims, not effNatural).
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
      {/* Canvas wrapper — sized to the PAINTED frame so box and pixels always agree in aspect
          (sizing from the live view while the async frame lags stretched the image during zoom). */}
      <div
        style={{
          position: "relative",
          width: boxW,
          height: boxH,
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
            width: boxW,
            height: boxH,
            borderRadius: 3,
            userSelect: "none",
            cursor: cropMode ? "default" : "grab",
          }}
        />

        {/* Overlays sit above canvas, sized + view-mapped to the PAINTED frame (matching pixels) */}
        {showOverlay && (
          <div
            style={{ position: "absolute", inset: 0, pointerEvents: "none" }}
          >
            <MaskOverlay
              width={boxW}
              height={boxH}
              viewRect={paintedView}
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
              width={boxW}
              height={boxH}
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

      {/* Render-failure overlay (auto-retry exhausted) — non-blocking, click to retry. */}
      {renderError && !rendering && (
        <div
          onClick={onRetryRender}
          title={renderError}
          style={{
            position: "absolute",
            top: "50%",
            left: "50%",
            transform: "translate(-50%, -50%)",
            padding: "8px 14px",
            borderRadius: "var(--radius-sm)",
            background: "rgba(0,0,0,.75)",
            color: "var(--color-t1, #eee)",
            fontSize: 12,
            fontFamily: "var(--font-mono)",
            cursor: "pointer",
            border: "1px solid rgba(255,255,255,.15)",
          }}
        >
          Render failed — click to retry
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
