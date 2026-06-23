import { useCallback, useEffect, useRef, useState } from "react";
import {
  thumbUrl,
  developGetEdit,
  developRender,
  effectivePreviewEdge,
} from "../../lib/ipc";
import type { DevelopParams, ImageRow, RenderedFrame } from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { freshDefaults } from "../../store/develop";
import type { DerivedView } from "../../lib/viewport";
import { useViewport } from "../../lib/useViewport";
import { paintFrame } from "../../lib/canvasPaint";

interface LoupeProps {
  image: ImageRow;
}

export default function Loupe({ image }: LoupeProps) {
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

  // ── Render state (tier-specific; the viewport machinery lives in useViewport) ───────────────────
  const savedParamsRef = useRef<DevelopParams | null>(null);
  const lastFrameRef = useRef<RenderedFrame | null>(null);
  // True while we're in full-res (develop_render) mode — used for zoom hysteresis so a zoom hovering
  // at the threshold doesn't thrash between the preview draw and the GPU decode.
  const decodeModeRef = useRef(false);

  // Draw a view sub-rect of the preview image straight to the canvas (instant; pan/light-zoom never
  // hit the backend). The preview is crop-applied, so view-uv [0,1] maps directly to its pixels.
  const drawPreview = useCallback(
    (d: DerivedView, canvas: HTMLCanvasElement) => {
      const img = previewImgRef.current;
      if (!img) return;
      const ctx = canvas.getContext("2d");
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
    },
    [],
  );

  const {
    containerRef,
    canvasRef,
    derived,
    scheduleRender,
    resetView,
    onPointerDown,
  } = useViewport<HTMLDivElement>({
    natural,
    resetKey: image.id,
    render: async (d, canvas) => {
      if (canvas.width !== d.outW || canvas.height !== d.outH) {
        canvas.width = d.outW;
        canvas.height = d.outH;
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
        drawPreview(d, canvas);
        return;
      }

      // Deep zoom → full-res render. Keep the (slightly soft) preview drawn underneath so there's
      // no blank flash while the decode runs; swap to the sharp frame when it lands.
      drawPreview(d, canvas);
      const p = savedParamsRef.current ?? freshDefaults();
      const frame = await developRender(
        image.id,
        p,
        d.view,
        d.outW,
        d.outH,
        -1,
      );
      if (frame) {
        lastFrameRef.current = frame;
        paintFrame(canvas, frame);
      }
    },
  });

  const { visCssW, visCssH } = derived;

  // ── Per-image load: saved params (for deep-zoom render) + the preview image ───────────────────
  // (viewState reset on image change is owned by useViewport via resetKey={image.id}.)
  useEffect(() => {
    let cancelled = false;
    previewImgRef.current = null;
    setPreviewDims(null);
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
        scheduleRender();
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
