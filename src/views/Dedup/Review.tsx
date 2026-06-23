import type { DupGroup, DupImage } from "../../lib/ipc";
import { CATEGORY_BADGE, CATEGORY_LABELS, groupTitle } from "./helpers";
import HeroStage from "./HeroStage";
import Filmstrip from "./Filmstrip";
import SignalsPanel from "./SignalsPanel";

export type ReviewMode = "compare" | "loupe" | "grid";

interface ReviewProps {
  group: DupGroup;
  groupIndex: number;
  groupCount: number;
  keeperId: number;
  focusId: number;
  mode: ReviewMode;
  canUndo: boolean;
  previewEdge: number;
  isStaged: (id: number) => boolean;
  /** Cache-bust token per image so an <img> refetches when its sharp preview lands. */
  tokenOf: (id: number) => number | undefined;
  setMode: (m: ReviewMode) => void;
  onBack: () => void;
  onPrevGroup: () => void;
  onNextGroup: () => void;
  onUndo: () => void;
  onAcceptBest: () => void;
  onFocus: (id: number) => void;
  onSetKeeper: (id: number) => void;
  onKeep: (id: number) => void;
  onReject: (id: number) => void;
  onRate: (img: DupImage, n: number) => void;
  onPreview: (img: DupImage) => void;
}

const MODES: ReviewMode[] = ["compare", "loupe", "grid"];

export default function Review({
  group,
  groupIndex,
  groupCount,
  keeperId,
  focusId,
  mode,
  canUndo,
  previewEdge,
  isStaged,
  tokenOf,
  setMode,
  onBack,
  onPrevGroup,
  onNextGroup,
  onUndo,
  onAcceptBest,
  onFocus,
  onSetKeeper,
  onKeep,
  onReject,
  onRate,
  onPreview,
}: ReviewProps) {
  const keeper = group.images.find((i) => i.id === keeperId) ?? group.images[0];
  const focus = group.images.find((i) => i.id === focusId) ?? group.images[0];

  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
      }}
    >
      {/* sub-header */}
      <div
        style={{
          height: 48,
          flex: "none",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "0 16px",
          borderBottom: "1px solid var(--color-line)",
          gap: 12,
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 12,
            minWidth: 0,
          }}
        >
          <button className="tbtn" onClick={onBack} style={{ fontSize: 12.5 }}>
            ← All groups
          </button>
          <div
            style={{ width: 1, height: 18, background: "var(--color-line-2)" }}
          />
          <span
            style={{
              fontSize: 10,
              fontWeight: 700,
              letterSpacing: ".07em",
              color: "var(--color-t1)",
              background: "var(--color-elev)",
              borderRadius: 4,
              padding: "3px 7px",
            }}
          >
            {CATEGORY_BADGE[group.category] ?? group.category.toUpperCase()}
          </span>
          <span style={{ fontSize: 12, color: "var(--color-t2)" }}>
            {CATEGORY_LABELS[group.category] ?? group.category} ·{" "}
            {group.images.length} files
          </span>
          <span style={{ fontSize: 12, color: "var(--color-t3)" }}>·</span>
          <span
            style={{
              fontSize: 12.5,
              color: "var(--color-t1)",
              fontWeight: 500,
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
            }}
          >
            {groupTitle(group)}
          </span>
        </div>

        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            flex: "none",
          }}
        >
          <button
            className="tbtn"
            onClick={onPrevGroup}
            disabled={groupIndex === 0}
          >
            ←
          </button>
          <span
            style={{
              fontSize: 12,
              color: "var(--color-t2)",
              fontFamily: "var(--font-mono)",
            }}
          >
            {groupIndex + 1} / {groupCount}
          </span>
          <button
            className="tbtn"
            onClick={onNextGroup}
            disabled={groupIndex >= groupCount - 1}
          >
            →
          </button>
          <div
            style={{ width: 1, height: 18, background: "var(--color-line-2)" }}
          />
          <button
            className="tbtn"
            onClick={onUndo}
            disabled={!canUndo}
            style={{ fontSize: 12, opacity: canUndo ? 1 : 0.4 }}
          >
            ↩ Undo
          </button>
          <div
            style={{
              display: "flex",
              background: "var(--color-stage)",
              border: "1px solid var(--color-line)",
              borderRadius: 7,
              padding: 2,
            }}
          >
            {MODES.map((m) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                style={{
                  padding: "4px 11px",
                  borderRadius: 5,
                  fontSize: 12,
                  fontWeight: 500,
                  textTransform: "capitalize",
                  border: "none",
                  cursor: "pointer",
                  color: mode === m ? "var(--color-t1)" : "var(--color-t2)",
                  background: mode === m ? "var(--color-hover)" : "transparent",
                }}
              >
                {m}
              </button>
            ))}
          </div>
          <button
            className="tbtn"
            onClick={onAcceptBest}
            style={{
              background: "var(--color-accent)",
              color: "#fff",
              fontWeight: 600,
              fontSize: 12.5,
            }}
          >
            Accept best{" "}
            <span style={{ opacity: 0.75, fontFamily: "var(--font-mono)" }}>
              ↵
            </span>
          </button>
        </div>
      </div>

      {/* body */}
      <div style={{ flex: 1, display: "flex", minHeight: 0 }}>
        <div
          style={{
            flex: 1,
            display: "flex",
            flexDirection: "column",
            minWidth: 0,
            background: "var(--color-stage)",
          }}
        >
          <HeroStage
            group={group}
            keeperId={keeperId}
            focusId={focusId}
            mode={mode}
            previewEdge={previewEdge}
            isStaged={isStaged}
            tokenOf={tokenOf}
            setMode={setMode}
            onFocus={onFocus}
            onSetKeeper={onSetKeeper}
            onReject={onReject}
            onPreview={onPreview}
          />
          <Filmstrip
            group={group}
            keeperId={keeperId}
            focusId={focusId}
            isStaged={isStaged}
            tokenOf={tokenOf}
            onFocus={onFocus}
            onKeep={onKeep}
            onReject={onReject}
          />
        </div>

        <SignalsPanel
          img={focus}
          keeper={keeper}
          group={group}
          isKeeper={focus.id === keeperId}
          isStaged={isStaged(focus.id)}
          onKeep={() => onKeep(focus.id)}
          onReject={() => onReject(focus.id)}
          onSetKeeper={() => onSetKeeper(focus.id)}
          onRate={(n) => onRate(focus, n)}
        />
      </div>
    </div>
  );
}
