import { useRef, useState, useCallback, useEffect } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

export interface GridImage {
  id: number;
  filename: string;
  gradient?: string;
  thumbUrl?: string;
  stars: number;
  flag: "pick" | "reject" | null;
  label?: string;
}

export interface SelectMods {
  meta: boolean;
  shift: boolean;
}

interface ThumbGridProps {
  images: GridImage[];
  thumbSize: number;
  selectedId: number | null;
  selectedIds: number[];
  onSelect: (id: number, mods: SelectMods) => void;
  /** Double-click / Enter activation — opens the full-size loupe preview. */
  onActivate?: (id: number) => void;
  /** Called when the user scrolls near the end of the loaded rows (infinite scroll). No-op when
   *  there is nothing more to load; the parent guards against concurrent/over-fetch. */
  onLoadMore?: () => void;
}

function StarRow({ count }: { count: number }) {
  return (
    <div style={{ display: "flex", gap: 1 }}>
      {[1, 2, 3, 4, 5].map((n) => (
        <svg
          key={n}
          viewBox="0 0 16 16"
          width={11}
          height={11}
          fill={n <= count ? "var(--color-star)" : "none"}
          stroke={n <= count ? "var(--color-star)" : "var(--color-t3)"}
          strokeWidth="1.2"
          style={{ display: "block" }}
        >
          <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
        </svg>
      ))}
    </div>
  );
}

export default function ThumbGrid({
  images,
  thumbSize,
  selectedId,
  selectedIds,
  onSelect,
  onActivate,
  onLoadMore,
}: ThumbGridProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [colCount, setColCount] = useState(4);
  const selectedSet = new Set(selectedIds);

  // Recompute column count when container width or thumbSize changes
  const updateCols = useCallback(() => {
    if (!containerRef.current) return;
    const w = containerRef.current.clientWidth - 28; // 14px padding each side
    const cols = Math.max(1, Math.floor((w + 12) / (thumbSize + 12)));
    setColCount(cols);
  }, [thumbSize]);

  useEffect(() => {
    updateCols();
    const ro = new ResizeObserver(updateCols);
    if (containerRef.current) ro.observe(containerRef.current);
    return () => ro.disconnect();
  }, [updateCols]);

  const rowCount = Math.ceil(images.length / colCount);
  const rowHeight = Math.round((thumbSize / 3) * 2) + 12; // 3:2 aspect + gap

  const rowVirtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => containerRef.current,
    estimateSize: () => rowHeight,
    overscan: 3,
  });

  const virtualRows = rowVirtualizer.getVirtualItems();

  // Infinite scroll: when the last rendered row is within a few rows of the loaded tail, ask the
  // parent for the next page. onLoadMore is parent-guarded (no-op when nothing more / already
  // loading), so firing it on every scroll frame is cheap.
  useEffect(() => {
    if (!onLoadMore) return;
    const last = virtualRows[virtualRows.length - 1];
    if (last && last.index >= rowCount - 3) onLoadMore();
  }, [virtualRows, rowCount, onLoadMore]);

  return (
    <div
      ref={containerRef}
      style={{
        height: "100%",
        overflowY: "auto",
        overflowX: "hidden",
        background: "var(--color-stage)",
      }}
    >
      <div
        style={{
          height: rowVirtualizer.getTotalSize(),
          position: "relative",
          padding: "14px",
        }}
      >
        {virtualRows.map((virtualRow) => {
          const startIdx = virtualRow.index * colCount;
          const rowImages = images.slice(startIdx, startIdx + colCount);
          return (
            <div
              key={virtualRow.index}
              style={{
                position: "absolute",
                top: virtualRow.start + 14,
                left: 14,
                right: 14,
                display: "grid",
                gridTemplateColumns: `repeat(${colCount}, 1fr)`,
                gap: 12,
              }}
            >
              {rowImages.map((img) => {
                const sel = selectedSet.has(img.id);
                const primary = img.id === selectedId;
                const hasOverlay = !!img.label || !!img.flag || img.stars > 0;
                return (
                  <div
                    key={img.id}
                    onClick={(e) =>
                      onSelect(img.id, {
                        meta: e.metaKey || e.ctrlKey,
                        shift: e.shiftKey,
                      })
                    }
                    onDoubleClick={() => onActivate?.(img.id)}
                    style={{
                      position: "relative",
                      aspectRatio: "3/2",
                      borderRadius: "var(--radius-sm)",
                      overflow: "hidden",
                      cursor: "pointer",
                      outline: sel
                        ? `2px solid ${primary ? "var(--color-accent)" : "var(--color-accent-line)"}`
                        : "1px solid var(--color-line)",
                      outlineOffset: 0,
                      background: "#000",
                    }}
                  >
                    {/* Placeholder / thumb */}
                    <div
                      style={{
                        position: "absolute",
                        inset: 0,
                        background: img.thumbUrl
                          ? `url(${img.thumbUrl}) center/cover`
                          : (img.gradient ?? "#1a1a1a"),
                      }}
                    />
                    {/* Inner vignette */}
                    <div
                      style={{
                        position: "absolute",
                        inset: 0,
                        boxShadow: "inset 0 0 40px rgba(0,0,0,.35)",
                        pointerEvents: "none",
                      }}
                    />
                    {/* Hover/selection overlay */}
                    <div
                      className={`cell-ov${sel || hasOverlay ? " visible" : ""}`}
                      style={{
                        position: "absolute",
                        inset: 0,
                        display: "flex",
                        flexDirection: "column",
                        justifyContent: "space-between",
                        padding: 7,
                        background:
                          "linear-gradient(180deg,rgba(0,0,0,.35),transparent 30%,transparent 60%,rgba(0,0,0,.55))",
                        transition: "opacity .12s",
                      }}
                    >
                      {/* Top row: label dot + flag */}
                      <div style={{ display: "flex", alignItems: "center" }}>
                        {img.label && (
                          <span
                            style={{
                              width: 9,
                              height: 9,
                              borderRadius: "50%",
                              background: img.label,
                              boxShadow: "0 0 0 2px rgba(0,0,0,.3)",
                              display: "block",
                            }}
                          />
                        )}
                        {img.flag === "pick" && (
                          <span
                            style={{
                              marginLeft: "auto",
                              fontSize: 11,
                              fontWeight: 600,
                              color: "var(--color-pick)",
                            }}
                          >
                            ●
                          </span>
                        )}
                        {img.flag === "reject" && (
                          <span
                            style={{
                              marginLeft: "auto",
                              fontSize: 11,
                              fontWeight: 600,
                              color: "var(--color-reject)",
                            }}
                          >
                            ⌀
                          </span>
                        )}
                      </div>
                      {/* Bottom row: stars + filename */}
                      <div
                        style={{
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "space-between",
                        }}
                      >
                        <StarRow count={img.stars} />
                        <span
                          style={{
                            fontFamily: "var(--font-mono)",
                            fontSize: 10,
                            color: "rgba(255,255,255,.78)",
                            textShadow: "0 1px 2px rgba(0,0,0,.6)",
                          }}
                        >
                          {img.filename}
                        </span>
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          );
        })}
      </div>
    </div>
  );
}
