import { useRef } from "react";
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
import { fitViewState, zoom1to1, type DerivedView } from "../../lib/viewport";
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

  // The rendered frame's pixel dims. In crop mode the backend renders the FULL frame (so the crop
  // overlay maps 1:1); otherwise it renders the cropped frame, whose aspect = sensor × crop extent.
  // Using sensor dims here when a crop is applied would distort the view rect + misalign overlays.
  const effNatural = cropMode
    ? natural
    : {
        w: Math.max(1, natural.w * crop.hw * 2),
        h: Math.max(1, natural.h * crop.hh * 2),
      };

  // Last successfully painted frame — for the resize-repaint below (avoids a wrong-scale flash).
  const lastFrameRef = useRef<RenderedFrame | null>(null);

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
        paintFrame(canvas, frame);
      }
    },
    resetKey: selectedId,
    renderDeps: [renderTick],
  });

  const { outW, outH, visCssW, visCssH } = derived;

  // Resize the canvas backing store to the derived size synchronously (e.g. on a container resize),
  // repainting the cached frame if its dims match — avoids a wrong-scale flash before the rAF render.
  const canvas = canvasRef.current;
  if (canvas && (canvas.width !== outW || canvas.height !== outH)) {
    canvas.width = outW;
    canvas.height = outH;
    const last = lastFrameRef.current;
    if (last && last.w === outW && last.h === outH) {
      const ctx = canvas.getContext("2d");
      if (ctx) ctx.putImageData(new ImageData(last.data, last.w, last.h), 0, 0);
    }
  }

  // Paint preview image when it arrives (instant first paint before the GPU render lands).
  const prevPreviewImg = useRef<HTMLImageElement | null>(null);
  if (previewImg && previewImg !== prevPreviewImg.current) {
    prevPreviewImg.current = previewImg;
    const c = canvasRef.current;
    if (c) {
      const ctx = c.getContext("2d");
      if (ctx) ctx.drawImage(previewImg, 0, 0, c.width, c.height);
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
