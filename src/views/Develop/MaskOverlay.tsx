import { useRef, useState } from "react";
import { clamp01, rgbToHsv } from "../../lib/maskGeom";
import { useDevelopStore } from "../../store/develop";
import type { BrushStroke, ComponentKind, Mask } from "../../lib/ipc";
import type { BrushSettings } from "../../store/develop";
import type { ViewRect } from "../../lib/viewport";

interface MaskOverlayProps {
  /** Visible image size in CSS pixels (the canvas element's CSS dimensions). */
  width: number;
  height: number;
  /** Current viewport window in crop-local UV. Used to map normalized coords ↔ screen px. */
  viewRect: ViewRect;
  mask: Mask;
  /** Which component of the mask is being edited. */
  compIndex: number;
  /** Replace the geometry of the active component. */
  onChangeKind: (kind: ComponentKind) => void;
  /** Current brush settings (for painting new strokes). */
  brush: BrushSettings;
  /** Commit a finished brush stroke to the mask. */
  onCommitStroke: (stroke: BrushStroke) => void;
}

const ACCENT = "var(--color-accent)";
const HANDLE_R = 7;

/** Map a crop-local UV coordinate to SVG/CSS pixels within the visible image rect. */
function normToPx(
  n: readonly [number, number],
  viewRect: ViewRect,
  w: number,
  h: number,
): [number, number] {
  return [
    ((n[0] - viewRect.ox) / viewRect.sx) * w,
    ((n[1] - viewRect.oy) / viewRect.sy) * h,
  ];
}

/** Map a client pointer position (within the SVG bounding rect) to crop-local UV. */
function clientToNormView(
  rect: DOMRect,
  clientX: number,
  clientY: number,
  viewRect: ViewRect,
): [number, number] {
  const fx = (clientX - rect.left) / Math.max(rect.width, 1);
  const fy = (clientY - rect.top) / Math.max(rect.height, 1);
  return [
    clamp01(viewRect.ox + fx * viewRect.sx),
    clamp01(viewRect.oy + fy * viewRect.sy),
  ];
}

/** Interactive handles for the selected mask's active component (linear or radial). Coordinates are
 *  stored in crop-local UV [0,1]; the SVG maps them to displayed-pixel space via viewRect. */
export default function MaskOverlay({
  width,
  height,
  viewRect,
  mask,
  compIndex,
  onChangeKind,
  brush,
  onCommitStroke,
}: MaskOverlayProps) {
  const svgRef = useRef<SVGSVGElement | null>(null);
  // Disarm the eyedropper while the crop tool is active — the stage shows the uncropped frame then,
  // so a pick would sample the wrong pixels (the develop preview is crop-relative).
  const picking = useDevelopStore((s) => s.pickingColor && !s.cropMode);
  const setPicking = useDevelopStore((s) => s.setPickingColor);
  const kind = mask.components[compIndex]?.kind;

  // Generic drag: while pressed, map pointer → crop-local UV and call `apply`.
  function startDrag(
    apply: (nx: number, ny: number) => void,
    e: React.PointerEvent,
  ) {
    e.stopPropagation();
    e.preventDefault();
    const move = (ev: PointerEvent) => {
      const rect = svgRef.current?.getBoundingClientRect();
      if (!rect) return;
      const [nx, ny] = clientToNormView(rect, ev.clientX, ev.clientY, viewRect);
      apply(nx, ny);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  if (!kind) return null;

  return (
    <svg
      ref={svgRef}
      width={width}
      height={height}
      style={{
        position: "absolute",
        inset: 0,
        overflow: "visible",
        pointerEvents: "auto",
      }}
    >
      {kind.type === "linear" && (
        <LinearHandles
          kind={kind}
          w={width}
          h={height}
          viewRect={viewRect}
          onChangeKind={onChangeKind}
          startDrag={startDrag}
        />
      )}
      {kind.type === "radial" && (
        <RadialHandles
          kind={kind}
          w={width}
          h={height}
          viewRect={viewRect}
          onChangeKind={onChangeKind}
          startDrag={startDrag}
        />
      )}
      {kind.type === "brush" && (
        <BrushLayer
          kind={kind}
          w={width}
          h={height}
          viewRect={viewRect}
          svgRef={svgRef}
          brush={brush}
          onCommitStroke={onCommitStroke}
        />
      )}
      {kind.type === "colorRange" && picking && (
        <rect
          x={0}
          y={0}
          width={width}
          height={height}
          fill="transparent"
          style={{ cursor: "crosshair" }}
          onPointerDown={(e) => {
            e.stopPropagation();
            const rect = svgRef.current?.getBoundingClientRect();
            if (!rect) return;
            const [nx, ny] = clientToNormView(
              rect,
              e.clientX,
              e.clientY,
              viewRect,
            );
            // Sample colour from the canvas element (the sibling below the SVG overlay)
            const canvas = rect
              ? (document.elementFromPoint(
                  rect.left + rect.width / 2,
                  rect.top + rect.height / 2,
                ) as HTMLCanvasElement | null)
              : null;
            void sampleCanvasHsv(canvas, nx, ny, viewRect).then(
              ({ hue, sat }) => {
                onChangeKind({ ...kind, hue, sat });
                setPicking(false);
              },
            );
          }}
        />
      )}
    </svg>
  );
}

/** Sample a pixel from the canvas at a crop-local UV position. */
async function sampleCanvasHsv(
  canvas: HTMLCanvasElement | null,
  nx: number,
  ny: number,
  viewRect: ViewRect,
): Promise<{ hue: number; sat: number }> {
  if (!canvas || canvas.tagName !== "CANVAS") {
    return { hue: 0.5, sat: 0.5 };
  }
  const ctx = canvas.getContext("2d");
  if (!ctx) return { hue: 0.5, sat: 0.5 };
  // Map crop-local UV → canvas px
  const fx = (nx - viewRect.ox) / Math.max(viewRect.sx, 1e-6);
  const fy = (ny - viewRect.oy) / Math.max(viewRect.sy, 1e-6);
  const px = Math.round(fx * canvas.width);
  const py = Math.round(fy * canvas.height);
  const d = ctx.getImageData(
    Math.max(0, Math.min(canvas.width - 1, px)),
    Math.max(0, Math.min(canvas.height - 1, py)),
    1,
    1,
  ).data;
  const [hue, sat] = rgbToHsv(d[0] / 255, d[1] / 255, d[2] / 255);
  return { hue, sat };
}

function BrushLayer({
  kind,
  w,
  h,
  viewRect,
  svgRef,
  brush,
  onCommitStroke,
}: {
  kind: Extract<ComponentKind, { type: "brush" }>;
  w: number;
  h: number;
  viewRect: ViewRect;
  svgRef: React.RefObject<SVGSVGElement | null>;
  brush: BrushSettings;
  onCommitStroke: (stroke: BrushStroke) => void;
}) {
  const [live, setLive] = useState<[number, number][] | null>(null);
  const ptsRef = useRef<[number, number][]>([]);
  // Stroke width in px: use the full-image CSS extent (w / viewRect.sx = full image CSS width)
  // so brush thickness tracks zoom correctly.
  const fullImgCssW = w / Math.max(viewRect.sx, 1e-6);
  const strokePx = Math.max(2, brush.size * fullImgCssW * 2);

  const begin = (e: React.PointerEvent) => {
    e.stopPropagation();
    e.preventDefault();
    const rect = svgRef.current?.getBoundingClientRect();
    if (!rect) return;
    ptsRef.current = [clientToNormView(rect, e.clientX, e.clientY, viewRect)];
    setLive([...ptsRef.current]);
    const move = (ev: PointerEvent) => {
      const r = svgRef.current?.getBoundingClientRect();
      if (!r) return;
      ptsRef.current.push(
        clientToNormView(r, ev.clientX, ev.clientY, viewRect),
      );
      setLive([...ptsRef.current]);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const points = ptsRef.current;
      if (points.length > 0) {
        onCommitStroke({
          points,
          size: brush.size,
          hardness: brush.hardness,
          flow: brush.flow,
          opacity: brush.opacity,
          isErase: brush.isErase,
        });
      }
      ptsRef.current = [];
      setLive(null);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  // Convert an array of crop-local UV points to an SVG path in canvas-px space
  const toPath = (pts: [number, number][]) =>
    pts
      .map((p, i) => {
        const [px, py] = normToPx(p, viewRect, w, h);
        return `${i === 0 ? "M" : "L"}${px} ${py}`;
      })
      .join(" ");

  const color = brush.isErase ? "#e25555" : ACCENT;

  return (
    <g>
      {/* Capture rect — paints, stops propagation so the canvas doesn't pan. */}
      <rect
        x={0}
        y={0}
        width={w}
        height={h}
        fill="transparent"
        style={{ cursor: "crosshair" }}
        onPointerDown={begin}
      />
      {/* Committed strokes (preview of painted coverage). */}
      {kind.strokes.map((s, i) => (
        <path
          key={i}
          d={toPath(s.points)}
          fill="none"
          stroke={s.isErase ? "#e25555" : ACCENT}
          strokeOpacity={0.28}
          strokeWidth={Math.max(2, s.size * fullImgCssW * 2)}
          strokeLinecap="round"
          strokeLinejoin="round"
          pointerEvents="none"
        />
      ))}
      {/* In-progress stroke. */}
      {live && live.length > 0 && (
        <path
          d={toPath(live)}
          fill="none"
          stroke={color}
          strokeOpacity={0.5}
          strokeWidth={strokePx}
          strokeLinecap="round"
          strokeLinejoin="round"
          pointerEvents="none"
        />
      )}
    </g>
  );
}

type DragStarter = (
  apply: (nx: number, ny: number) => void,
  e: React.PointerEvent,
) => void;

function Handle({
  x,
  y,
  onDown,
  cursor = "grab",
}: {
  x: number;
  y: number;
  onDown: (e: React.PointerEvent) => void;
  cursor?: string;
}) {
  return (
    <g onPointerDown={onDown} style={{ cursor }}>
      <circle cx={x} cy={y} r={HANDLE_R + 4} fill="transparent" />
      <circle
        cx={x}
        cy={y}
        r={HANDLE_R}
        fill="rgba(0,0,0,.35)"
        stroke="#fff"
        strokeWidth={2}
      />
    </g>
  );
}

function LinearHandles({
  kind,
  w,
  h,
  viewRect,
  onChangeKind,
  startDrag,
}: {
  kind: Extract<ComponentKind, { type: "linear" }>;
  w: number;
  h: number;
  viewRect: ViewRect;
  onChangeKind: (k: ComponentKind) => void;
  startDrag: DragStarter;
}) {
  const p0 = kind.p0;
  const p1 = kind.p1;
  const [x0, y0] = normToPx(p0, viewRect, w, h);
  const [x1, y1] = normToPx(p1, viewRect, w, h);

  // Perpendicular guide lines extended across the frame
  const ax = x1 - x0;
  const ay = y1 - y0;
  const len = Math.hypot(ax, ay) || 1;
  const px = (-ay / len) * Math.max(w, h);
  const py = (ax / len) * Math.max(w, h);

  const moveBoth = (nx: number, ny: number, anchor: "p0" | "p1") => {
    if (anchor === "p0") {
      const dx = nx - p0[0];
      const dy = ny - p0[1];
      onChangeKind({
        ...kind,
        p0: [clamp01(nx), clamp01(ny)],
        p1: [clamp01(p1[0] + dx), clamp01(p1[1] + dy)],
      });
    }
  };

  return (
    <g>
      <line
        x1={x0 - px}
        y1={y0 - py}
        x2={x0 + px}
        y2={y0 + py}
        stroke={ACCENT}
        strokeWidth={1.5}
        opacity={0.9}
      />
      <line
        x1={x1 - px}
        y1={y1 - py}
        x2={x1 + px}
        y2={y1 + py}
        stroke={ACCENT}
        strokeWidth={1.2}
        strokeDasharray="5 4"
        opacity={0.7}
      />
      <line
        x1={x0}
        y1={y0}
        x2={x1}
        y2={y1}
        stroke={ACCENT}
        strokeWidth={1.5}
        opacity={0.8}
      />
      <Handle
        x={x0}
        y={y0}
        onDown={(e) => startDrag((nx, ny) => moveBoth(nx, ny, "p0"), e)}
      />
      <Handle
        x={x1}
        y={y1}
        onDown={(e) =>
          startDrag(
            (nx, ny) =>
              onChangeKind({ ...kind, p1: [clamp01(nx), clamp01(ny)] }),
            e,
          )
        }
      />
    </g>
  );
}

function RadialHandles({
  kind,
  w,
  h,
  viewRect,
  onChangeKind,
  startDrag,
}: {
  kind: Extract<ComponentKind, { type: "radial" }>;
  w: number;
  h: number;
  viewRect: ViewRect;
  onChangeKind: (k: ComponentKind) => void;
  startDrag: DragStarter;
}) {
  const [cx, cy] = kind.center;
  const [rx, ry] = kind.radius;
  const [px, py] = normToPx([cx, cy], viewRect, w, h);
  // Radius in px: map a point offset by the radius from the center
  const [prx] = normToPx([cx + rx, cy], viewRect, w, h);
  const [, pry] = normToPx([cx, cy + ry], viewRect, w, h);
  const erx = Math.abs(prx - px);
  const ery = Math.abs(pry - py);

  return (
    <g>
      <ellipse
        cx={px}
        cy={py}
        rx={erx}
        ry={ery}
        fill="none"
        stroke={ACCENT}
        strokeWidth={1.6}
        opacity={0.9}
      />
      {/* Center: move. */}
      <Handle
        x={px}
        y={py}
        cursor="move"
        onDown={(e) =>
          startDrag(
            (nx, ny) =>
              onChangeKind({ ...kind, center: [clamp01(nx), clamp01(ny)] }),
            e,
          )
        }
      />
      {/* Right: set X radius. */}
      <Handle
        x={px + erx}
        y={py}
        cursor="ew-resize"
        onDown={(e) =>
          startDrag(
            (nx) =>
              onChangeKind({
                ...kind,
                radius: [Math.max(0.01, Math.abs(nx - cx)), ry],
              }),
            e,
          )
        }
      />
      {/* Bottom: set Y radius. */}
      <Handle
        x={px}
        y={py + ery}
        cursor="ns-resize"
        onDown={(e) =>
          startDrag(
            (_nx, ny) =>
              onChangeKind({
                ...kind,
                radius: [rx, Math.max(0.01, Math.abs(ny - cy))],
              }),
            e,
          )
        }
      />
    </g>
  );
}
