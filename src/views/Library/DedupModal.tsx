import { useState, useEffect, useCallback } from "react";
import { useAppStore } from "../../store/app";
import {
  dedupScan,
  dedupResolve,
  thumbUrl,
  type DupGroup,
} from "../../lib/ipc";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function formatDate(ts: number | null): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

interface Props {
  open: boolean;
  onClose: () => void;
  onRefresh: () => void;
}

export default function DedupModal({ open, onClose, onRefresh }: Props) {
  const setToast = useAppStore((s) => s.setToast);
  const [groups, setGroups] = useState<DupGroup[]>([]);
  const [loading, setLoading] = useState(false);
  // keeperId per group key
  const [keepers, setKeepers] = useState<Record<string, number>>({});

  const scan = useCallback(async () => {
    setLoading(true);
    try {
      const [byByte, byCapture] = await Promise.all([
        dedupScan("byte"),
        dedupScan("capture"),
      ]);
      // Merge; byte-exact dups take precedence. Avoid duplicate group keys.
      const seen = new Set<string>();
      const merged: DupGroup[] = [];
      for (const g of [...byByte, ...byCapture]) {
        if (!seen.has(g.key)) {
          seen.add(g.key);
          merged.push(g);
        }
      }

      if (merged.length === 0) {
        setToast("No duplicates found");
        onClose();
        return;
      }

      setGroups(merged);
      // Default keeper = first image in each group
      const defaults: Record<string, number> = {};
      for (const g of merged) {
        if (g.images[0]) defaults[g.key] = g.images[0].id;
      }
      setKeepers(defaults);
    } catch (err) {
      setToast(`Scan failed: ${String(err)}`);
      onClose();
    } finally {
      setLoading(false);
    }
  }, [setToast, onClose]);

  useEffect(() => {
    if (open) void scan();
  }, [open, scan]);

  // Keyboard dismiss
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onClose]);

  const handleTrash = useCallback(
    async (group: DupGroup) => {
      const keepId = keepers[group.key] ?? group.images[0]?.id;
      if (keepId === undefined) return;
      const trashIds = group.images
        .map((img) => img.id)
        .filter((id) => id !== keepId);
      try {
        const count = await dedupResolve(keepId, trashIds);
        setToast(`Trashed ${count} duplicate${count === 1 ? "" : "s"}`);
        setGroups((prev) => prev.filter((g) => g.key !== group.key));
        onRefresh();
      } catch (err) {
        setToast(`Resolve failed: ${String(err)}`);
      }
    },
    [keepers, setToast, onRefresh],
  );

  if (!open) return null;

  return (
    <div
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,.45)",
        backdropFilter: "blur(2px)",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        paddingTop: "8vh",
        zIndex: 50,
      }}
    >
      <div
        style={{
          width: 720,
          maxWidth: "94vw",
          maxHeight: "80vh",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.6)",
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        {/* Header */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            padding: "14px 16px",
            borderBottom: "1px solid var(--color-line)",
            flexShrink: 0,
          }}
        >
          <span
            style={{
              fontWeight: 600,
              fontSize: 13.5,
              color: "var(--color-t1)",
            }}
          >
            Find Duplicates
          </span>
          <button
            onClick={onClose}
            style={{
              color: "var(--color-t3)",
              fontSize: 18,
              lineHeight: 1,
              padding: "0 4px",
            }}
            aria-label="Close"
          >
            ×
          </button>
        </div>

        {/* Body */}
        <div style={{ overflowY: "auto", flex: 1, padding: "12px 0" }}>
          {loading && (
            <div
              style={{
                padding: "32px 16px",
                textAlign: "center",
                color: "var(--color-t3)",
                fontSize: 13,
              }}
            >
              Scanning…
            </div>
          )}

          {!loading && groups.length === 0 && (
            <div
              style={{
                padding: "32px 16px",
                textAlign: "center",
                color: "var(--color-t3)",
                fontSize: 13,
              }}
            >
              All duplicates resolved.
            </div>
          )}

          {!loading &&
            groups.map((group) => {
              const keepId = keepers[group.key] ?? group.images[0]?.id;
              return (
                <div
                  key={group.key}
                  style={{
                    marginBottom: 1,
                    borderBottom: "1px solid var(--color-line)",
                    padding: "10px 16px",
                  }}
                >
                  {/* Group header */}
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "space-between",
                      marginBottom: 10,
                    }}
                  >
                    <span
                      style={{
                        fontSize: 10,
                        fontWeight: 600,
                        letterSpacing: ".06em",
                        textTransform: "uppercase",
                        color: "var(--color-t3)",
                      }}
                    >
                      {group.category === "byte"
                        ? "Exact duplicate"
                        : "Same capture time"}{" "}
                      · {group.images.length} files
                    </span>
                    <button
                      className="tbtn ghost"
                      onClick={() => void handleTrash(group)}
                      style={{ fontSize: 11.5, padding: "3px 10px" }}
                    >
                      Trash others
                    </button>
                  </div>

                  {/* Images */}
                  <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
                    {group.images.map((img) => {
                      const isKeeper = img.id === keepId;
                      return (
                        <label
                          key={img.id}
                          style={{
                            display: "flex",
                            flexDirection: "column",
                            gap: 6,
                            cursor: "pointer",
                            width: 130,
                          }}
                        >
                          {/* Thumbnail */}
                          <div
                            style={{
                              position: "relative",
                              width: 130,
                              height: 88,
                              borderRadius: "var(--radius-sm)",
                              overflow: "hidden",
                              border: isKeeper
                                ? "2px solid var(--color-accent)"
                                : "2px solid var(--color-line)",
                              background: "var(--color-elev)",
                            }}
                          >
                            <img
                              src={thumbUrl(img.contentHash, 256)}
                              alt={img.filename}
                              style={{
                                width: "100%",
                                height: "100%",
                                objectFit: "cover",
                              }}
                            />
                            {isKeeper && (
                              <div
                                style={{
                                  position: "absolute",
                                  top: 4,
                                  right: 4,
                                  background: "var(--color-accent)",
                                  color: "#fff",
                                  fontSize: 9,
                                  fontWeight: 700,
                                  letterSpacing: ".04em",
                                  borderRadius: 3,
                                  padding: "2px 5px",
                                }}
                              >
                                KEEP
                              </div>
                            )}
                          </div>

                          {/* Meta */}
                          <div
                            style={{
                              fontSize: 11,
                              color: "var(--color-t2)",
                              lineHeight: 1.4,
                            }}
                          >
                            <div
                              style={{
                                overflow: "hidden",
                                textOverflow: "ellipsis",
                                whiteSpace: "nowrap",
                                color: "var(--color-t1)",
                                fontFamily: "var(--font-mono)",
                                fontSize: 10.5,
                              }}
                              title={img.filename}
                            >
                              {img.filename}
                            </div>
                            <div>{formatBytes(img.fileSize)}</div>
                            <div>{formatDate(img.captureDate)}</div>
                          </div>

                          {/* Radio */}
                          <div
                            style={{
                              display: "flex",
                              alignItems: "center",
                              gap: 5,
                            }}
                          >
                            <input
                              type="radio"
                              name={`keeper-${group.key}`}
                              checked={isKeeper}
                              onChange={() =>
                                setKeepers((prev) => ({
                                  ...prev,
                                  [group.key]: img.id,
                                }))
                              }
                              style={{
                                accentColor: "var(--color-accent)",
                                cursor: "pointer",
                              }}
                            />
                            <span
                              style={{ fontSize: 11, color: "var(--color-t3)" }}
                            >
                              Keep
                            </span>
                          </div>
                        </label>
                      );
                    })}
                  </div>
                </div>
              );
            })}
        </div>

        {/* Footer */}
        {!loading && groups.length > 0 && (
          <div
            style={{
              display: "flex",
              alignItems: "center",
              justifyContent: "space-between",
              padding: "10px 16px",
              borderTop: "1px solid var(--color-line)",
              flexShrink: 0,
            }}
          >
            <span style={{ fontSize: 12, color: "var(--color-t3)" }}>
              {groups.length} group{groups.length === 1 ? "" : "s"} remaining
            </span>
            <button
              className="tbtn ghost"
              onClick={onClose}
              style={{ fontSize: 12 }}
            >
              Close
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
