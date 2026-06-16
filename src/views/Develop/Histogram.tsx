import { useDevelopStore } from "../../store/develop";

const W = 276;
const H = 96;

// Max bin excluding the pure-black/white spikes (which would otherwise flatten everything).
function scaleMax(...channels: number[][]): number {
  let m = 1;
  for (const bins of channels) {
    for (let i = 1; i < bins.length - 1; i++) {
      if (bins[i] > m) m = bins[i];
    }
  }
  return m;
}

// Filled area path for one channel; sqrt-scaled for visibility.
function areaPath(bins: number[], maxv: number): string {
  const n = bins.length;
  let d = `M0 ${H} `;
  for (let i = 0; i < n; i++) {
    const x = (i / (n - 1)) * W;
    const v = Math.min(1, Math.sqrt(bins[i] / maxv));
    d += `L${x.toFixed(1)} ${(H - v * H).toFixed(1)} `;
  }
  return d + `L${W} ${H} Z`;
}

function meanPct(bins: number[]): number {
  let sum = 0;
  let count = 0;
  for (let i = 0; i < bins.length; i++) {
    sum += i * bins[i];
    count += bins[i];
  }
  if (count === 0) return 0;
  return Math.round((sum / count / 255) * 100);
}

export default function Histogram() {
  const hist = useDevelopStore((s) => s.histogram);

  const maxv = hist ? scaleMax(hist.r, hist.g, hist.b) : 1;
  const channels: [string, number[] | null][] = [
    ["#c56d6d", hist?.r ?? null],
    ["#6db074", hist?.g ?? null],
    ["#5b93cf", hist?.b ?? null],
  ];

  return (
    <div
      style={{
        padding: "14px 14px 10px",
        borderBottom: "1px solid var(--color-line)",
      }}
    >
      <div
        style={{
          height: 96,
          borderRadius: "var(--radius-sm)",
          background: "var(--color-stage-dev)",
          outline: "1px solid var(--color-line)",
          position: "relative",
          overflow: "hidden",
        }}
      >
        <svg
          width="100%"
          height="100%"
          viewBox={`0 0 ${W} ${H}`}
          preserveAspectRatio="none"
        >
          {channels.map(([color, bins], i) =>
            bins ? (
              <path
                key={i}
                d={areaPath(bins, maxv)}
                fill={color}
                opacity={0.55}
                style={{ mixBlendMode: "screen" }}
              />
            ) : null,
          )}
        </svg>
        {!hist && (
          <span
            style={{
              position: "absolute",
              inset: 0,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              fontSize: 10.5,
              color: "var(--color-t3)",
            }}
          >
            no signal
          </span>
        )}
      </div>
      <div
        style={{
          display: "flex",
          gap: 14,
          marginTop: 8,
          fontFamily: "var(--font-mono)",
          fontSize: 10.5,
          color: "var(--color-t3)",
        }}
      >
        <span>
          R{" "}
          <b style={{ fontWeight: 400, color: "var(--color-t2)" }}>
            {hist ? `${meanPct(hist.r)}%` : "—"}
          </b>
        </span>
        <span>
          G{" "}
          <b style={{ fontWeight: 400, color: "var(--color-t2)" }}>
            {hist ? `${meanPct(hist.g)}%` : "—"}
          </b>
        </span>
        <span>
          B{" "}
          <b style={{ fontWeight: 400, color: "var(--color-t2)" }}>
            {hist ? `${meanPct(hist.b)}%` : "—"}
          </b>
        </span>
      </div>
    </div>
  );
}
