import { useRef, useState, useEffect } from "react";
import type { CurvePoint, ToneCurve as ToneCurveData } from "../../lib/ipc";

const W = 248;
const H = 160;
const P = 10;
const MAX_POINTS = 16; // Lightroom parity
const EPS = 0.001; // min x-gap between neighbours so ordering/index stay stable during a drag

function sx(t: number) {
  return P + t * (W - 2 * P);
}
function sy(v: number) {
  return H - P - v * (H - 2 * P);
}
function clamp01(v: number) {
  return Math.max(0, Math.min(1, v));
}

// Identity curve = the two endpoints only (a straight diagonal). An unedited channel shows this and
// nothing is persisted until the user edits (backend treats empty/collinear as identity).
const IDENTITY: CurvePoint[] = [
  { x: 0, y: 0 },
  { x: 1, y: 1 },
];

// --- Monotone cubic Hermite (Fritsch–Carlson) — a faithful port of `core-pipeline::curve::eval`, so
// the drawn line matches the LUT actually applied to the image (WYSIWYG). ----------------------------
function cleanPts(points: CurvePoint[]): [number, number][] {
  const p = points.map(
    (c) => [clamp01(c.x), clamp01(c.y)] as [number, number],
  );
  p.sort((a, b) => a[0] - b[0]);
  const out: [number, number][] = [];
  for (const q of p) {
    if (out.length && Math.abs(q[0] - out[out.length - 1][0]) < 1e-6) continue;
    out.push(q);
  }
  return out;
}

function evalClean(pts: [number, number][], x: number): number {
  const n = pts.length;
  if (n === 0) return clamp01(x);
  if (n === 1) return pts[0][1];
  if (x <= pts[0][0]) return pts[0][1];
  if (x >= pts[n - 1][0]) return pts[n - 1][1];

  const d: number[] = [];
  for (let i = 0; i < n - 1; i++) {
    d.push((pts[i + 1][1] - pts[i][1]) / (pts[i + 1][0] - pts[i][0]));
  }
  const m = new Array<number>(n).fill(0);
  m[0] = d[0];
  m[n - 1] = d[n - 2];
  for (let i = 1; i < n - 1; i++) m[i] = (d[i - 1] + d[i]) * 0.5;

  for (let i = 0; i < n - 1; i++) {
    if (Math.abs(d[i]) < 1e-9) {
      m[i] = 0;
      m[i + 1] = 0;
    } else {
      const a = m[i] / d[i];
      const b = m[i + 1] / d[i];
      const s = a * a + b * b;
      if (s > 9) {
        const t = 3 / Math.sqrt(s);
        m[i] = t * a * d[i];
        m[i + 1] = t * b * d[i];
      }
    }
  }

  let k = 0;
  while (k < n - 1 && x > pts[k + 1][0]) k++;
  const h = pts[k + 1][0] - pts[k][0];
  const t = (x - pts[k][0]) / h;
  const t2 = t * t;
  const t3 = t2 * t;
  const h00 = 2 * t3 - 3 * t2 + 1;
  const h10 = t3 - 2 * t2 + t;
  const h01 = -2 * t3 + 3 * t2;
  const h11 = t3 - t2;
  return clamp01(
    h00 * pts[k][1] + h10 * h * m[k] + h01 * pts[k + 1][1] + h11 * h * m[k + 1],
  );
}

// Sample the monotone spline across the plot width → a polyline path (matches the applied LUT).
function buildCurve(points: CurvePoint[]): string {
  const clean = cleanPts(points);
  const N = 128;
  let d = "";
  for (let i = 0; i <= N; i++) {
    const t = i / N;
    const y = evalClean(clean, t);
    d += `${i === 0 ? "M" : "L"}${sx(t).toFixed(2)} ${sy(y).toFixed(2)} `;
  }
  return d.trim();
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
  // A persisted curve has its points; otherwise show the editable straight identity.
  const pts = stored.length >= 2 ? stored : IDENTITY;

  // Hold the latest drag inputs in a ref so the global listeners attach ONCE (below) rather than
  // re-binding on every curve edit / param change.
  const latest = useRef({ pts, key, onChange });
  latest.current = { pts, key, onChange };

  // Map a pointer event to a normalized {x,y} in [0,1] (accounts for the CSS-scaled svg).
  const pointerToValue = (e: {
    clientX: number;
    clientY: number;
  }): CurvePoint | null => {
    const svg = svgRef.current;
    if (!svg) return null;
    const rect = svg.getBoundingClientRect();
    const scaleX = W / rect.width;
    const scaleY = H / rect.height;
    const rawX = ((e.clientX - rect.left) * scaleX - P) / (W - 2 * P);
    const rawY = 1 - ((e.clientY - rect.top) * scaleY - P) / (H - 2 * P);
    return { x: clamp01(rawX), y: clamp01(rawY) };
  };

  useEffect(() => {
    const handlePointerMove = (e: PointerEvent) => {
      const idx = draggingIdx.current;
      if (idx === null) return;
      const v = pointerToValue(e);
      if (!v) return;
      const { pts, key, onChange } = latest.current;
      const isEnd = idx === 0 || idx === pts.length - 1;
      // Endpoints stay pinned in x (0 / 1); interior points move in x, clamped between neighbours.
      let x = pts[idx].x;
      if (!isEnd) {
        const left = pts[idx - 1].x + EPS;
        const right = pts[idx + 1].x - EPS;
        x = Math.min(right, Math.max(left, v.x));
      }
      const next = pts.map((pt, i) => (i === idx ? { x, y: v.y } : pt));
      onChange(key, next);
    };
    const handlePointerUp = () => {
      draggingIdx.current = null;
    };
    window.addEventListener("pointermove", handlePointerMove);
    window.addEventListener("pointerup", handlePointerUp);
    return () => {
      window.removeEventListener("pointermove", handlePointerMove);
      window.removeEventListener("pointerup", handlePointerUp);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Click on empty plot → insert a control point at the cursor and start dragging it.
  const handleAddPoint = (e: React.PointerEvent<SVGSVGElement>) => {
    const { pts, key, onChange } = latest.current;
    if (pts.length >= MAX_POINTS) return;
    const v = pointerToValue(e);
    if (!v) return;
    // Keep x-sorted and always between the two fixed endpoints.
    let insert = pts.findIndex((p) => p.x > v.x);
    if (insert <= 0) insert = 1;
    if (insert >= pts.length) insert = pts.length - 1;
    const next = [...pts.slice(0, insert), v, ...pts.slice(insert)];
    draggingIdx.current = insert;
    svgRef.current?.setPointerCapture(e.pointerId);
    onChange(key, next);
  };

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
        style={{ display: "block", cursor: "crosshair", touchAction: "none" }}
        onPointerDown={handleAddPoint}
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
        {pts.map((pt, i) => {
          const isEnd = i === 0 || i === pts.length - 1;
          return (
            <circle
              key={i}
              cx={sx(pt.x)}
              cy={sy(pt.y)}
              r={4}
              fill={isEnd ? "var(--color-t3)" : "var(--color-accent)"}
              stroke="var(--color-stage-dev)"
              strokeWidth={2}
              style={{ cursor: isEnd ? "ns-resize" : "move" }}
              onPointerDown={(e) => {
                e.stopPropagation(); // don't also add a point via the svg handler
                draggingIdx.current = i;
                (e.currentTarget as SVGCircleElement).setPointerCapture(
                  e.pointerId,
                );
              }}
              onDoubleClick={(e) => {
                e.stopPropagation();
                if (isEnd) return; // endpoints are not removable
                onChange(
                  key,
                  pts.filter((_, j) => j !== i),
                );
              }}
            />
          );
        })}
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
