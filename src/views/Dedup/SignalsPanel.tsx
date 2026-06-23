import type { DupGroup, DupImage } from "../../lib/ipc";
import {
  decisionSignals,
  fmtBytes,
  fmtDate,
  fmtExif,
  keeperScore,
  scoreColor,
} from "./helpers";

interface SignalsPanelProps {
  img: DupImage;
  keeper: DupImage;
  group: DupGroup;
  isKeeper: boolean;
  isStaged: boolean;
  onKeep: () => void;
  onReject: () => void;
  onSetKeeper: () => void;
  onRate: (n: number) => void;
}

function ScoreRing({ score }: { score: number }) {
  const color = scoreColor(score);
  return (
    <div style={{ position: "relative", width: 64, height: 64, flex: "none" }}>
      <svg
        viewBox="0 0 36 36"
        width={64}
        height={64}
        style={{ transform: "rotate(-90deg)" }}
      >
        <circle
          cx="18"
          cy="18"
          r="15.9"
          fill="none"
          stroke="rgba(255,255,255,.08)"
          strokeWidth="3.2"
        />
        <circle
          cx="18"
          cy="18"
          r="15.9"
          fill="none"
          stroke={color}
          strokeWidth="3.2"
          strokeLinecap="round"
          pathLength={100}
          strokeDasharray={`${score} 100`}
        />
      </svg>
      <div
        style={{
          position: "absolute",
          inset: 0,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontSize: 18,
          fontWeight: 700,
          color: "var(--color-t1)",
        }}
      >
        {score}
      </div>
    </div>
  );
}

const SECTION: React.CSSProperties = {
  padding: "14px 16px",
  borderBottom: "1px solid var(--color-line)",
};

const CAPTION: React.CSSProperties = {
  fontSize: 11,
  color: "var(--color-t3)",
  textTransform: "uppercase",
  letterSpacing: ".06em",
};

export default function SignalsPanel({
  img,
  keeper,
  group,
  isKeeper,
  isStaged,
  onKeep,
  onReject,
  onSetKeeper,
  onRate,
}: SignalsPanelProps) {
  const score = keeperScore(img, group);
  const signals = decisionSignals(img, keeper, group);

  const verdictText = isKeeper
    ? "Current keeper for this group."
    : isStaged
      ? "Staged for the bin — restore to keep."
      : score >= keeperScore(keeper, group)
        ? "Scores at or above the keeper — consider promoting."
        : "Lower keeper-fit than the current keeper.";

  return (
    <div
      style={{
        width: 328,
        flex: "none",
        borderLeft: "1px solid var(--color-line)",
        background: "var(--color-panel)",
        display: "flex",
        flexDirection: "column",
        overflowY: "auto",
      }}
    >
      {/* File identity */}
      <div
        style={{
          padding: "15px 16px",
          borderBottom: "1px solid var(--color-line)",
        }}
      >
        <div
          style={{
            fontFamily: "var(--font-mono)",
            fontSize: 13,
            color: "var(--color-t1)",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
          title={img.filename}
        >
          {img.filename}
        </div>
        <div style={{ fontSize: 11.5, color: "var(--color-t2)", marginTop: 3 }}>
          {fmtExif(img)}
        </div>
        <div style={{ fontSize: 11.5, color: "var(--color-t3)", marginTop: 2 }}>
          {fmtBytes(img.fileSize)} · {fmtDate(img.captureDate)}
        </div>
      </div>

      {/* Score + verdict */}
      <div
        style={{ ...SECTION, display: "flex", gap: 16, alignItems: "center" }}
      >
        <ScoreRing score={score} />
        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ ...CAPTION, marginBottom: 3 }}>Keeper fit</div>
          {isKeeper && (
            <span
              style={{
                display: "inline-block",
                fontSize: 9.5,
                fontWeight: 700,
                letterSpacing: ".05em",
                color: "var(--color-pick)",
                border: "1px solid var(--color-pick)",
                borderRadius: 4,
                padding: "1px 6px",
              }}
            >
              KEEPER
            </span>
          )}
          <div
            style={{ fontSize: 11.5, color: "var(--color-t2)", marginTop: 6 }}
          >
            {verdictText}
          </div>
        </div>
      </div>

      {/* Verdict buttons + rating */}
      <div style={SECTION}>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            onClick={onKeep}
            className="tbtn"
            style={{
              flex: 1,
              justifyContent: "center",
              background: isStaged ? "transparent" : "var(--color-pick)",
              color: isStaged ? "var(--color-t1)" : "#0e1a10",
              fontWeight: 600,
            }}
          >
            ✓ Keep
          </button>
          <button
            onClick={onReject}
            className="tbtn"
            disabled={isKeeper}
            style={{
              flex: 1,
              justifyContent: "center",
              background: isStaged ? "var(--color-reject)" : "transparent",
              color: isStaged ? "#1a0e0e" : "var(--color-t1)",
              border: isStaged ? "none" : "1px solid var(--color-line-2)",
              fontWeight: 600,
              opacity: isKeeper ? 0.4 : 1,
            }}
          >
            ✕ Reject
          </button>
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 8,
            marginTop: 11,
          }}
        >
          <span style={{ fontSize: 11, color: "var(--color-t3)" }}>Rating</span>
          <div style={{ display: "flex", gap: 3 }}>
            {[1, 2, 3, 4, 5].map((n) => (
              <span
                key={n}
                onClick={() => onRate(n)}
                style={{
                  cursor: "pointer",
                  fontSize: 15,
                  color:
                    n <= img.stars
                      ? "var(--color-star)"
                      : "var(--color-line-2)",
                }}
              >
                ★
              </span>
            ))}
          </div>
          {!isKeeper && (
            <button
              onClick={onSetKeeper}
              className="tbtn"
              style={{
                marginLeft: "auto",
                fontSize: 11.5,
                color: "var(--color-accent)",
                padding: "2px 4px",
              }}
            >
              Set as keeper
            </button>
          )}
        </div>
      </div>

      {/* Decision signals */}
      <div style={SECTION}>
        <div style={{ ...CAPTION, marginBottom: 11 }}>
          Decision signals{" "}
          <span style={{ textTransform: "none", letterSpacing: 0 }}>
            · vs keeper
          </span>
        </div>
        {signals.map((m) => (
          <div key={m.key} style={{ marginBottom: 11 }}>
            <div
              style={{
                display: "flex",
                justifyContent: "space-between",
                alignItems: "center",
                marginBottom: 4,
              }}
            >
              <span style={{ fontSize: 12, color: "var(--color-t2)" }}>
                {m.label}
              </span>
              <span style={{ display: "flex", alignItems: "center", gap: 7 }}>
                {m.delta && (
                  <span
                    style={{
                      fontSize: 11,
                      fontFamily: "var(--font-mono)",
                      color: m.delta.good
                        ? "var(--color-pick)"
                        : "var(--color-reject)",
                    }}
                  >
                    {m.delta.label}
                  </span>
                )}
                <span
                  style={{
                    fontFamily: "var(--font-mono)",
                    fontSize: 12,
                    color: "var(--color-t1)",
                  }}
                >
                  {m.valLabel}
                </span>
              </span>
            </div>
            <div
              style={{
                height: 6,
                background: "rgba(255,255,255,.07)",
                borderRadius: 3,
                overflow: "hidden",
              }}
            >
              <div
                style={{
                  height: "100%",
                  width: `${Math.round(m.barFrac * 100)}%`,
                  background: isKeeper
                    ? "var(--color-accent)"
                    : "rgba(233,233,236,.45)",
                  borderRadius: 3,
                }}
              />
            </div>
          </div>
        ))}
      </div>

      <div
        style={{
          padding: "12px 16px",
          fontSize: 10.5,
          color: "var(--color-t3)",
          lineHeight: 1.5,
        }}
      >
        Signals are derived from capture metadata. Pixel-level quality, faces
        and exposure analysis are not yet available.
      </div>
    </div>
  );
}
