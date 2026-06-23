import { useEffect, useRef, useState } from "react";
import { thumbUrl, type DupGroup, type DupImage } from "../../lib/ipc";
import { fmtExif, keeperScore } from "./helpers";
import { Badge, borderColor, Caption } from "./frameVisual";
import type { ReviewMode } from "./Review";

interface HeroStageProps {
  group: DupGroup;
  keeperId: number;
  focusId: number;
  mode: ReviewMode;
  previewEdge: number;
  isStaged: (id: number) => boolean;
  tokenOf: (id: number) => number | undefined;
  setMode: (m: ReviewMode) => void;
  onFocus: (id: number) => void;
  onSetKeeper: (id: number) => void;
  onReject: (id: number) => void;
  onPreview: (img: DupImage) => void;
}

function gridBtn(bg: string, color: string): React.CSSProperties {
  return {
    border: "none",
    cursor: "pointer",
    fontSize: 10.5,
    fontWeight: 600,
    padding: "3px 8px",
    borderRadius: 5,
    background: bg,
    color,
  };
}

function scoreChip(value: number): React.ReactElement {
  return (
    <div
      style={{
        position: "absolute",
        top: 10,
        right: 10,
        fontFamily: "var(--font-mono)",
        fontSize: 11,
        color: "#fff",
        background: "rgba(0,0,0,.55)",
        padding: "3px 7px",
        borderRadius: 5,
      }}
    >
      {value}
    </div>
  );
}

export default function HeroStage({
  group,
  keeperId,
  focusId,
  mode,
  previewEdge,
  isStaged,
  tokenOf,
  setMode,
  onFocus,
  onSetKeeper,
  onReject,
  onPreview,
}: HeroStageProps) {
  const [flickOn, setFlickOn] = useState(false);
  const [flickB, setFlickB] = useState(false);
  const [zoom, setZoom] = useState(false);
  const [pos, setPos] = useState({ x: 50, y: 50 });
  const flickTimer = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    setFlickOn(false);
    setZoom(false);
  }, [group.key]);

  useEffect(() => {
    if (flickOn)
      flickTimer.current = setInterval(() => setFlickB((b) => !b), 450);
    return () => {
      if (flickTimer.current) clearInterval(flickTimer.current);
    };
  }, [flickOn]);

  // F = A/B flicker (compare), Z = zoom (loupe). Other keys are owned by DedupView.
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (e.metaKey || e.ctrlKey) return;
      if (e.key === "f" || e.key === "F") {
        setMode("compare");
        setFlickOn((f) => !f);
      } else if (e.key === "z" || e.key === "Z") {
        setMode("loupe");
        setZoom((z) => !z);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [setMode]);

  const keeper = group.images.find((i) => i.id === keeperId) ?? group.images[0];
  const focus = group.images.find((i) => i.id === focusId) ?? group.images[0];
  const challenger =
    focus.id === keeper.id
      ? (group.images.find((i) => i.id !== keeper.id) ?? focus)
      : focus;

  const onHeroMove = (e: React.MouseEvent) => {
    if (!zoom) return;
    const r = e.currentTarget.getBoundingClientRect();
    setPos({
      x: ((e.clientX - r.left) / r.width) * 100,
      y: ((e.clientY - r.top) / r.height) * 100,
    });
  };

  function pane(img: DupImage, opts: { onClick?: () => void; hint?: string }) {
    return (
      <div
        onClick={opts.onClick}
        style={{
          flex: 1,
          position: "relative",
          borderRadius: 9,
          overflow: "hidden",
          minWidth: 0,
          background: "var(--color-stage)",
          cursor: opts.onClick ? "pointer" : "default",
        }}
      >
        <img
          src={thumbUrl(
            img.contentHash,
            1024,
            null,
            tokenOf(img.id),
            previewEdge,
          )}
          alt={img.filename}
          style={{
            position: "absolute",
            inset: 0,
            width: "100%",
            height: "100%",
            objectFit: "contain",
          }}
          loading="lazy"
        />
        <div
          style={{
            position: "absolute",
            inset: 0,
            borderRadius: 9,
            border: `2px solid ${borderColor(img, keeperId, focusId, isStaged)}`,
            pointerEvents: "none",
          }}
        />
        <Badge img={img} keeperId={keeperId} isStaged={isStaged} />
        {scoreChip(keeperScore(img, group))}
        <Caption img={img} exif={fmtExif(img)} />
        {opts.hint && (
          <div
            style={{
              position: "absolute",
              right: 10,
              bottom: 12,
              fontSize: 11,
              color: "rgba(255,255,255,.7)",
              background: "rgba(0,0,0,.4)",
              padding: "4px 8px",
              borderRadius: 6,
            }}
          >
            {opts.hint}
          </div>
        )}
      </div>
    );
  }

  return (
    <>
      <div style={{ flex: 1, minHeight: 0, padding: 18, display: "flex" }}>
        {mode === "compare" &&
          (flickOn ? (
            <div style={{ flex: 1, display: "flex", minWidth: 0 }}>
              {pane(flickB ? challenger : keeper, {})}
            </div>
          ) : (
            <div style={{ flex: 1, display: "flex", gap: 14, minWidth: 0 }}>
              {pane(keeper, {})}
              {pane(challenger, {
                onClick: () => onSetKeeper(challenger.id),
                hint: "Click → make keeper",
              })}
            </div>
          ))}

        {mode === "loupe" && (
          <div
            onMouseMove={onHeroMove}
            onClick={() => setZoom((z) => !z)}
            style={{
              flex: 1,
              position: "relative",
              borderRadius: 9,
              overflow: "hidden",
              minWidth: 0,
              background: "var(--color-stage)",
              cursor: zoom ? "zoom-out" : "zoom-in",
            }}
          >
            <img
              src={thumbUrl(
                focus.contentHash,
                1024,
                null,
                tokenOf(focus.id),
                previewEdge,
              )}
              alt={focus.filename}
              style={{
                position: "absolute",
                inset: 0,
                width: "100%",
                height: "100%",
                objectFit: "contain",
                transform: zoom ? "scale(2.4)" : "scale(1)",
                transformOrigin: `${pos.x}% ${pos.y}%`,
                transition: zoom ? "none" : "transform .12s",
              }}
            />
            <div
              style={{
                position: "absolute",
                inset: 0,
                borderRadius: 9,
                border: `2px solid ${borderColor(focus, keeperId, focusId, isStaged)}`,
                pointerEvents: "none",
              }}
            />
            <Badge img={focus} keeperId={keeperId} isStaged={isStaged} />
            <div
              style={{
                position: "absolute",
                top: 10,
                right: 10,
                fontSize: 11,
                color: "#fff",
                background: "rgba(0,0,0,.55)",
                padding: "4px 8px",
                borderRadius: 5,
              }}
            >
              {zoom ? "Fit" : "100%"}
            </div>
            <Caption img={focus} exif={fmtExif(focus)} />
          </div>
        )}

        {mode === "grid" && (
          <div style={{ flex: 1, overflowY: "auto", minWidth: 0 }}>
            <div
              style={{
                display: "grid",
                gridTemplateColumns: "repeat(auto-fill,minmax(220px,1fr))",
                gap: 14,
              }}
            >
              {group.images.map((img) => (
                <div
                  key={img.id}
                  onClick={() => onFocus(img.id)}
                  style={{
                    position: "relative",
                    borderRadius: 8,
                    overflow: "hidden",
                    cursor: "pointer",
                    background: "var(--color-stage)",
                    aspectRatio: "3 / 2",
                  }}
                >
                  <img
                    src={thumbUrl(img.contentHash, 512, null, tokenOf(img.id))}
                    alt={img.filename}
                    style={{
                      width: "100%",
                      height: "100%",
                      objectFit: "contain",
                      display: "block",
                    }}
                    loading="lazy"
                  />
                  <div
                    style={{
                      position: "absolute",
                      inset: 0,
                      borderRadius: 8,
                      border: `2px solid ${borderColor(img, keeperId, focusId, isStaged)}`,
                      pointerEvents: "none",
                    }}
                  />
                  <Badge
                    img={img}
                    keeperId={keeperId}
                    isStaged={isStaged}
                    showNeutral={false}
                  />
                  <div
                    style={{
                      position: "absolute",
                      left: 0,
                      right: 0,
                      bottom: 0,
                      padding: "18px 9px 7px",
                      background: "linear-gradient(transparent,rgba(0,0,0,.8))",
                      display: "flex",
                      justifyContent: "space-between",
                      alignItems: "center",
                      gap: 6,
                    }}
                  >
                    <span
                      style={{
                        fontFamily: "var(--font-mono)",
                        fontSize: 10.5,
                        color: "#fff",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                      }}
                    >
                      {img.filename}
                    </span>
                    <div style={{ display: "flex", gap: 5, flex: "none" }}>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          onSetKeeper(img.id);
                        }}
                        style={gridBtn("var(--color-pick)", "#0e1a10")}
                      >
                        Keep
                      </button>
                      <button
                        onClick={(e) => {
                          e.stopPropagation();
                          onReject(img.id);
                        }}
                        disabled={img.id === keeperId}
                        style={{
                          ...gridBtn("rgba(0,0,0,.55)", "#fff"),
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
          </div>
        )}
      </div>

      {/* compare controls */}
      {mode === "compare" && (
        <div
          style={{
            flex: "none",
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            gap: 8,
            padding: "0 18px 10px",
          }}
        >
          <button
            className="tbtn ghost"
            onClick={() => onPreview(challenger)}
            style={{ fontSize: 12 }}
          >
            ⤢ Full size
          </button>
          <button
            className="tbtn ghost"
            onClick={() => setFlickOn((f) => !f)}
            style={{
              fontSize: 12,
              color: flickOn ? "var(--color-t1)" : undefined,
              background: flickOn ? "var(--color-hover)" : undefined,
            }}
          >
            A / B flicker{" "}
            <span style={{ opacity: 0.6, fontFamily: "var(--font-mono)" }}>
              F
            </span>
          </button>
          <button
            className="tbtn"
            onClick={() => onSetKeeper(challenger.id)}
            style={{
              background: "var(--color-pick)",
              color: "#0e1a10",
              fontWeight: 600,
              fontSize: 12,
            }}
          >
            Make keeper{" "}
            <span style={{ opacity: 0.6, fontFamily: "var(--font-mono)" }}>
              S
            </span>
          </button>
        </div>
      )}
    </>
  );
}
