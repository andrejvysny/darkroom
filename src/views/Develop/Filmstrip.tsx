import { useState } from "react";
import Icon from "../../components/Icon";

const PH = [
  "linear-gradient(160deg,#3a4a5c,#1d2733 70%)",
  "linear-gradient(160deg,#6b5340,#2a211a 70%)",
  "radial-gradient(120% 90% at 30% 20%,#7a6a4a,#2b2519 75%)",
  "linear-gradient(160deg,#41584f,#1a2420 70%)",
  "linear-gradient(200deg,#8a6a5a,#33241f 70%)",
  "radial-gradient(120% 100% at 70% 30%,#566a82,#1c242e 75%)",
  "linear-gradient(160deg,#5a5560,#23212a 70%)",
  "linear-gradient(150deg,#9a7e5a,#3a2e1f 72%)",
  "radial-gradient(120% 90% at 40% 80%,#3f5560,#161d20 75%)",
  "linear-gradient(170deg,#705a6a,#271f25 70%)",
  "linear-gradient(160deg,#4a6157,#1b241f 70%)",
];

const FRAMES = Array.from({ length: 22 }, (_, i) => ({
  id: i,
  gradient: PH[i % PH.length],
  current: i === 6,
}));

const STARS = [1, 1, 1, 1, 0];

export default function Filmstrip() {
  const [currentFrame, setCurrentFrame] = useState(6);
  const [zoom, setZoom] = useState(38);

  return (
    <footer
      style={{
        display: "flex",
        alignItems: "center",
        gap: 14,
        padding: "0 14px",
        background: "var(--color-app)",
        borderTop: "1px solid var(--color-line)",
        height: 84,
        flexShrink: 0,
      }}
    >
      {/* Meta: stars + pick flag */}
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <div style={{ display: "flex", gap: 1 }}>
          {STARS.map((filled, i) => (
            <svg
              key={i}
              viewBox="0 0 16 16"
              width={13}
              height={13}
              fill={filled ? "var(--color-star)" : "none"}
              stroke={filled ? "var(--color-star)" : "var(--color-t3)"}
              strokeWidth="1.2"
              style={{ display: "block" }}
            >
              <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
            </svg>
          ))}
        </div>
        <span style={{ fontSize: 12, color: "var(--color-pick)" }}>●</span>
      </div>

      {/* Strip */}
      <div
        style={{
          flex: 1,
          display: "flex",
          gap: 8,
          overflowX: "auto",
          padding: "14px 0",
          height: 84,
          alignItems: "center",
          scrollbarWidth: "none",
        }}
      >
        {FRAMES.map((frame) => (
          <div
            key={frame.id}
            onClick={() => setCurrentFrame(frame.id)}
            style={{
              height: 54,
              aspectRatio: "3/2",
              borderRadius: 4,
              flexShrink: 0,
              outline:
                frame.id === currentFrame
                  ? "2px solid var(--color-accent)"
                  : "1px solid var(--color-line)",
              position: "relative",
              cursor: "pointer",
              overflow: "hidden",
              background: frame.gradient,
            }}
          >
            <div
              style={{
                position: "absolute",
                inset: 0,
                boxShadow: "inset 0 0 18px rgba(0,0,0,.4)",
              }}
            />
          </div>
        ))}
      </div>

      {/* Right: zoom + 1:1 */}
      <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 8,
            fontFamily: "var(--font-mono)",
            fontSize: 11.5,
            color: "var(--color-t2)",
          }}
        >
          <Icon
            name="zoom"
            size={13}
            style={{ color: "var(--color-t3)" } as React.CSSProperties}
          />
          <input
            type="range"
            min={10}
            max={200}
            value={zoom}
            onChange={(e) => setZoom(Number(e.target.value))}
            style={{ width: 90 }}
          />
          <span>Fit</span>
        </div>
        <button
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "5px 10px",
            border: "1px solid var(--color-line)",
            borderRadius: "var(--radius-sm)",
            fontSize: 12,
            color: "var(--color-t2)",
          }}
        >
          <Icon name="split" size={13} />
          1:1
        </button>
      </div>
    </footer>
  );
}
