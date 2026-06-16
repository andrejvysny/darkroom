import { useRef, useState, useCallback, useEffect } from "react";
import type { CurvePoint, ToneCurve as ToneCurveData } from "../../lib/ipc";

const W = 248;
const H = 160;
const P = 10;

function sx(t: number) {
  return P + t * (W - 2 * P);
}
function sy(v: number) {
  return H - P - v * (H - 2 * P);
}

// Editable identity curve (x = y): 4 points so the middle two can shape an S-curve.
const IDENTITY: CurvePoint[] = [
  { x: 0, y: 0 },
  { x: 0.33, y: 0.33 },
  { x: 0.66, y: 0.66 },
  { x: 1, y: 1 },
];

function buildCurve(pts: CurvePoint[]): string {
  let d = `M${sx(pts[0].x)} ${sy(pts[0].y)}`;
  for (let i = 1; i < pts.length; i++) {
    const p0 = pts[i - 1];
    const p1 = pts[i];
    const cx = (sx(p0.x) + sx(p1.x)) / 2;
    d += ` C ${cx} ${sy(p0.y)} ${cx} ${sy(p1.y)} ${sx(p1.x)} ${sy(p1.y)}`;
  }
  return d;
}

function buildHistBackdrop(): string {
  let d = `M${P} ${H - P} `;
  for (let x = P; x <= W - P; x += 4) {
    const t = (x - P) / (W - 2 * P);
    const v = Math.exp(-Math.pow((t - 0.45) / 0.28, 2));
    d += `L${x} ${(H - P - v * (H - 2 * P) * 0.8).toFixed(1)} `;
  }
  return d + `L${W - P} ${H - P} Z`;
}

const GRID_LINES = [1, 2, 3].flatMap((i) => [
  {
    x1: P + (W - 2 * P) * (i / 4),
    y1: P,
    x2: P + (W - 2 * P) * (i / 4),
    y2: H - P,
  },
  {
    x1: P,
    y1: P + (H - 2 * P) * (i / 4),
    x2: W - P,
    y2: P + (H - 2 * P) * (i / 4),
  },
]);

type CurveChannel = "RGB" | "R" | "G" | "B";
const TABS: CurveChannel[] = ["RGB", "R", "G", "B"];
const CHANNEL_KEY: Record<CurveChannel, keyof ToneCurveData> = {
  RGB: "rgb",
  R: "r",
  G: "g",
  B: "b",
};
const CHANNEL_COLOR: Record<CurveChannel, string> = {
  RGB: "var(--color-t1)",
  R: "#d66",
  G: "#6c6",
  B: "#69d",
};

interface ToneCurveProps {
  curve: ToneCurveData;
  onChange: (channel: keyof ToneCurveData, points: CurvePoint[]) => void;
}

export default function ToneCurve({ curve, onChange }: ToneCurveProps) {
  const [channel, setChannel] = useState<CurveChannel>("RGB");
  const svgRef = useRef<SVGSVGElement>(null);
  const draggingIdx = useRef<number | null>(null);

  const key = CHANNEL_KEY[channel];
  const stored = curve[key];
  // A persisted curve has its points; otherwise show the editable identity.
  const pts = stored.length >= 2 ? stored : IDENTITY;

  const handlePointerMove = useCallback(
    (e: PointerEvent) => {
      const idx = draggingIdx.current;
      if (idx === null || !svgRef.current) return;
      const rect = svgRef.current.getBoundingClientRect();
      const scaleY = H / rect.height;
      const rawV = 1 - ((e.clientY - rect.top) * scaleY - P) / (H - 2 * P);
      const v = Math.max(0, Math.min(1, rawV));
      const next = pts.map((pt, i) => (i === idx ? { ...pt, y: v } : pt));
      onChange(key, next);
    },
    [pts, key, onChange],
  );

  const handlePointerUp = useCallback(() => {
    draggingIdx.current = null;
  }, []);

  useEffect(() => {
    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
  }, [handlePointerMove, handlePointerUp]);

  return (
    <div
      style={{
        border: "1px solid var(--color-line)",
        borderRadius: "var(--radius-sm)",
        background: "var(--color-stage-dev)",
        overflow: "hidden",
      }}
    >
      <svg
        ref={svgRef}
        viewBox={`0 0 ${W} ${H}`}
        width="100%"
        style={{ display: "block" }}
      >
        {GRID_LINES.map((l, i) => (
          <line
            key={i}
            x1={l.x1}
            y1={l.y1}
            x2={l.x2}
            y2={l.y2}
            stroke="rgba(255,255,255,.05)"
          />
        ))}
        <path d={buildHistBackdrop()} fill="rgba(255,255,255,.06)" />
        <path
          d={buildCurve(pts)}
          fill="none"
          stroke={CHANNEL_COLOR[channel]}
          strokeWidth="1.6"
        />
        {pts.map((pt, i) => (
          <circle
            key={i}
            cx={sx(pt.x)}
            cy={sy(pt.y)}
            r={4}
            fill={
              i === 0 || i === pts.length - 1
                ? "var(--color-t3)"
                : "var(--color-accent)"
            }
            stroke="var(--color-stage-dev)"
            strokeWidth={2}
            style={{ cursor: "ns-resize" }}
            onPointerDown={(e) => {
              draggingIdx.current = i;
              (e.currentTarget as SVGCircleElement).setPointerCapture(
                e.pointerId,
              );
            }}
          />
        ))}
      </svg>
      <div style={{ display: "flex", gap: 2, padding: 6 }}>
        {TABS.map((tab) => (
          <button
            key={tab}
            onClick={() => setChannel(tab)}
            style={{
              flex: 1,
              padding: 3,
              borderRadius: 4,
              fontSize: 11,
              color: channel === tab ? "var(--color-t1)" : "var(--color-t3)",
              background: channel === tab ? "var(--color-elev)" : "transparent",
            }}
          >
            {tab}
          </button>
        ))}
      </div>
    </div>
  );
}
