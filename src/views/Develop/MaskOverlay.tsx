import { useRef, useState } from "react";
import { clamp01, clientToNorm, samplePixelHsv } from "../../lib/maskGeom";
import { useDevelopStore } from "../../store/develop";
import type { BrushStroke, ComponentKind, Mask } from "../../lib/ipc";
import type { BrushSettings } from "../../store/develop";

interface MaskOverlayProps {
  /** Displayed image size in CSS pixels (overlay draws in this space; scales with stage zoom). */
  width: number;
  height: number;
  mask: Mask;
  /** Which component of the mask is being edited. */
  compIndex: number;
  /** Replace the geometry of the active component. */
  onChangeKind: (kind: ComponentKind) => void;
  /** Current brush settings (for painting new strokes). */
  brush: BrushSettings;
  /** Commit a finished brush stroke to the mask. */
  onCommitStroke: (stroke: BrushStroke) => void;
  /** Displayed image URL (for the color-range eyedropper). */
  imageUrl: string | null;
}

const ACCENT = "var(--color-accent)";
const HANDLE_R = 7;

/** Interactive handles for the selected mask's first component (linear or radial). Coordinates are
 *  normalized [0,1]; the SVG draws them in displayed-pixel space and follows stage zoom/pan. */
export default function MaskOverlay({
  width,
  height,
  mask,
  compIndex,
  onChangeKind,
  brush,
  onCommitStroke,
  imageUrl,
}: MaskOverlayProps) {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const picking = useDevelopStore((s) => s.pickingColor);
  const setPicking = useDevelopStore((s) => s.setPickingColor);
  const kind = mask.components[compIndex]?.kind;

  // Generic drag: while pressed, map pointer → normalized and call `apply` to derive a new kind.
  function startDrag(
    apply: (nx: number, ny: number) => void,
    e: React.PointerEvent,
  ) {
    e.stopPropagation();
    e.preventDefault();
    const move = (ev: PointerEvent) => {
      const rect = svgRef.current?.getBoundingClientRect();
      if (!rect) return;
      const [nx, ny] = clientToNorm(rect, ev.clientX, ev.clientY);
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
      style={{ position: "absolute", inset: 0, overflow: "visible" }}
    >
      {kind.type === "linear" && (
        <LinearHandles
          kind={kind}
          w={width}
          h={height}
          onChangeKind={onChangeKind}
          startDrag={startDrag}
        />
      )}
      {kind.type === "radial" && (
        <RadialHandles
          kind={kind}
          w={width}
          h={height}
          onChangeKind={onChangeKind}
          startDrag={startDrag}
        />
      )}
      {kind.type === "brush" && (
        <BrushLayer
          kind={kind}
          w={width}
          h={height}
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
            if (!rect || !imageUrl) return;
            const [nx, ny] = clientToNorm(rect, e.clientX, e.clientY);
            void samplePixelHsv(imageUrl, nx, ny).then(({ hue, sat }) => {
              onChangeKind({ ...kind, hue, sat });
              setPicking(false);
            });
          }}
        />
      )}
    </svg>
  );
}

function BrushLayer({
  kind,
  w,
  h,
  svgRef,
  brush,
  onCommitStroke,
}: {
  kind: Extract<ComponentKind, { type: "brush" }>;
  w: number;
  h: number;
  svgRef: React.RefObject<SVGSVGElement | null>;
  brush: BrushSettings;
  onCommitStroke: (stroke: BrushStroke) => void;
}) {
  const [live, setLive] = useState<[number, number][] | null>(null);
  const ptsRef = useRef<[number, number][]>([]);
  const L = Math.max(w, h);
  const strokePx = Math.max(2, brush.size * L * 2);

  const begin = (e: React.PointerEvent) => {
    e.stopPropagation();
    e.preventDefault();
    const rect = svgRef.current?.getBoundingClientRect();
    if (!rect) return;
    ptsRef.current = [clientToNorm(rect, e.clientX, e.clientY)];
    setLive([...ptsRef.current]);
    const move = (ev: PointerEvent) => {
      const r = svgRef.current?.getBoundingClientRect();
      if (!r) return;
      ptsRef.current.push(clientToNorm(r, ev.clientX, ev.clientY));
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

  const toPath = (pts: [number, number][]) =>
    pts
      .map((p, i) => `${i === 0 ? "M" : "L"}${p[0] * w} ${p[1] * h}`)
      .join(" ");

  const color = brush.isErase ? "#e25555" : ACCENT;

  return (
    <g>
      {/* Capture rect — paints, and stops propagation so the stage doesn't pan. */}
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
          strokeWidth={Math.max(2, s.size * L * 2)}
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
  onChangeKind,
  startDrag,
}: {
  kind: Extract<ComponentKind, { type: "linear" }>;
  w: number;
  h: number;
  onChangeKind: (k: ComponentKind) => void;
  startDrag: DragStarter;
}) {
  const p0 = kind.p0;
  const p1 = kind.p1;
  const x0 = p0[0] * w;
  const y0 = p0[1] * h;
  const x1 = p1[0] * w;
  const y1 = p1[1] * h;

  // Perpendicular guide lines at p0 (full effect) and p1 (no effect), extended across the frame.
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
      {/* Drag the axis line (via the p0 endpoint) to translate the whole gradient. */}
      <Handle
        x={x0}
        y={y0}
        onDown={(e) => startDrag((nx, ny) => moveBoth(nx, ny, "p0"), e)}
      />
      {/* Drag p1 to set direction/spread. */}
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
  onChangeKind,
  startDrag,
}: {
  kind: Extract<ComponentKind, { type: "radial" }>;
  w: number;
  h: number;
  onChangeKind: (k: ComponentKind) => void;
  startDrag: DragStarter;
}) {
  const [cx, cy] = kind.center;
  const [rx, ry] = kind.radius;
  const px = cx * w;
  const py = cy * h;
  const erx = rx * w;
  const ery = ry * h;

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
