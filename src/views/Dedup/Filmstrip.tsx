import { thumbUrl, type DupGroup } from "../../lib/ipc";
import { keeperScore } from "./helpers";
import { borderColor } from "./frameVisual";

interface FilmstripProps {
  group: DupGroup;
  keeperId: number;
  focusId: number;
  isStaged: (id: number) => boolean;
  tokenOf: (id: number) => number | undefined;
  onFocus: (id: number) => void;
  onKeep: (id: number) => void;
  onReject: (id: number) => void;
}

function stripBtn(color: string): React.CSSProperties {
  return {
    border: "none",
    background: "none",
    cursor: "pointer",
    fontSize: 11,
    fontWeight: 700,
    color,
    padding: 0,
    lineHeight: 1,
  };
}

const HINTS = [
  "← → Frame",
  "Space Keep",
  "X Reject",
  "↵ Accept best",
  "[ ] Group",
  "C / L / G View",
  "Z Zoom",
  "F Flicker",
  "S Set keeper",
  "1–5 Rate",
];

export default function Filmstrip({
  group,
  keeperId,
  focusId,
  isStaged,
  tokenOf,
  onFocus,
  onKeep,
  onReject,
}: FilmstripProps) {
  return (
    <div
      style={{
        flex: "none",
        borderTop: "1px solid var(--color-line)",
        background: "var(--color-app)",
      }}
    >
      <div
        style={{
          display: "flex",
          gap: 9,
          padding: "11px 14px",
          overflowX: "auto",
        }}
      >
        {group.images.map((img, i) => (
          <div
            key={img.id}
            onClick={() => onFocus(img.id)}
            style={{ flex: "none", width: 132, cursor: "pointer" }}
          >
            <div
              style={{
                position: "relative",
                width: 132,
                height: 84,
                borderRadius: 7,
                overflow: "hidden",
                background: "var(--color-stage)",
              }}
            >
              <img
                src={thumbUrl(img.contentHash, 256, null, tokenOf(img.id))}
                alt={img.filename}
                style={{
                  width: "100%",
                  height: "100%",
                  objectFit: "cover",
                  display: "block",
                }}
                loading="lazy"
              />
              <div
                style={{
                  position: "absolute",
                  inset: 0,
                  borderRadius: 7,
                  border: `2px solid ${borderColor(img, keeperId, focusId, isStaged)}`,
                  pointerEvents: "none",
                }}
              />
              <div
                style={{
                  position: "absolute",
                  top: 5,
                  left: 5,
                  fontFamily: "var(--font-mono)",
                  fontSize: 9.5,
                  color: "#fff",
                  background: "rgba(0,0,0,.55)",
                  padding: "1px 5px",
                  borderRadius: 3,
                }}
              >
                #{i + 1}
              </div>
              <div
                style={{
                  position: "absolute",
                  left: 0,
                  right: 0,
                  bottom: 0,
                  height: 24,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  padding: "0 5px",
                  background: "linear-gradient(transparent,rgba(0,0,0,.72))",
                }}
              >
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onKeep(img.id);
                  }}
                  style={stripBtn(
                    img.id === keeperId ? "var(--color-pick)" : "#fff",
                  )}
                >
                  K
                </button>
                <span
                  style={{
                    fontFamily: "var(--font-mono)",
                    fontSize: 9.5,
                    color: "#fff",
                  }}
                >
                  {keeperScore(img, group)}
                </span>
                <button
                  onClick={(e) => {
                    e.stopPropagation();
                    onReject(img.id);
                  }}
                  disabled={img.id === keeperId}
                  style={{
                    ...stripBtn(
                      isStaged(img.id) ? "var(--color-reject)" : "#fff",
                    ),
                    opacity: img.id === keeperId ? 0.4 : 1,
                  }}
                >
                  ✕
                </button>
              </div>
            </div>
          </div>
        ))}
      </div>
      <div
        style={{
          display: "flex",
          gap: 16,
          padding: "0 14px 9px",
          fontSize: 11,
          color: "var(--color-t3)",
          flexWrap: "wrap",
        }}
      >
        {HINTS.map((h) => (
          <span
            key={h}
            style={
              h.includes("Accept best")
                ? { color: "var(--color-accent)" }
                : undefined
            }
          >
            {h}
          </span>
        ))}
      </div>
    </div>
  );
}
