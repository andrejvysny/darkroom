import { useState } from "react";
import { thumbUrl } from "../../lib/ipc";
import type { StagedItem } from "../../store/dedup";
import { CATEGORY_LABELS, fmtBytes } from "./helpers";

interface BinDrawerProps {
  open: boolean;
  staged: StagedItem[];
  busy: boolean;
  onClose: () => void;
  onRestore: (id: number) => void;
  /** Move every staged photo to the OS trash (calls dedupResolve per group). */
  onEmpty: () => void;
}

export default function BinDrawer({
  open,
  staged,
  busy,
  onClose,
  onRestore,
  onEmpty,
}: BinDrawerProps) {
  const [confirm, setConfirm] = useState(false);
  if (!open) return null;

  const totalBytes = staged.reduce((s, it) => s + it.image.fileSize, 0);

  return (
    <>
      <div
        onClick={onClose}
        style={{
          position: "fixed",
          inset: 0,
          background: "rgba(0,0,0,.4)",
          zIndex: 40,
        }}
      />
      <div
        style={{
          position: "fixed",
          top: 0,
          right: 0,
          bottom: 0,
          width: 380,
          background: "var(--color-panel)",
          borderLeft: "1px solid var(--color-line-2)",
          zIndex: 41,
          display: "flex",
          flexDirection: "column",
          boxShadow: "-12px 0 40px rgba(0,0,0,.4)",
        }}
      >
        <div
          style={{
            padding: "15px 16px",
            borderBottom: "1px solid var(--color-line)",
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <div>
            <div style={{ fontSize: 14, fontWeight: 600 }}>
              Staged for deletion
            </div>
            <div
              style={{ fontSize: 11.5, color: "var(--color-t2)", marginTop: 2 }}
            >
              {staged.length === 0
                ? "Empty"
                : `${staged.length} photo${staged.length === 1 ? "" : "s"} · ${fmtBytes(totalBytes)}`}
            </div>
          </div>
          <button
            onClick={onClose}
            className="tbtn"
            style={{ fontSize: 18, color: "var(--color-t3)", padding: "0 6px" }}
          >
            ×
          </button>
        </div>

        <div style={{ flex: 1, overflowY: "auto", padding: "8px 0" }}>
          {staged.length === 0 ? (
            <div
              style={{
                padding: "40px 20px",
                textAlign: "center",
                color: "var(--color-t3)",
                fontSize: 12.5,
                lineHeight: 1.6,
              }}
            >
              Nothing staged. Rejected photos land here first — safe until you
              empty the bin.
            </div>
          ) : (
            staged.map((it) => (
              <div
                key={it.image.id}
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 11,
                  padding: "8px 16px",
                }}
              >
                <div
                  style={{
                    position: "relative",
                    width: 64,
                    height: 42,
                    borderRadius: "var(--radius-sm)",
                    overflow: "hidden",
                    flex: "none",
                    background: "var(--color-stage)",
                  }}
                >
                  <img
                    src={thumbUrl(it.image.contentHash, 256)}
                    alt={it.image.filename}
                    style={{
                      width: "100%",
                      height: "100%",
                      objectFit: "cover",
                      display: "block",
                    }}
                    loading="lazy"
                  />
                </div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div
                    style={{
                      fontFamily: "var(--font-mono)",
                      fontSize: 11.5,
                      color: "var(--color-t1)",
                      overflow: "hidden",
                      textOverflow: "ellipsis",
                      whiteSpace: "nowrap",
                    }}
                  >
                    {it.image.filename}
                  </div>
                  <div style={{ fontSize: 11, color: "var(--color-t2)" }}>
                    {fmtBytes(it.image.fileSize)} ·{" "}
                    {CATEGORY_LABELS[it.category] ?? it.category}
                  </div>
                </div>
                <button
                  onClick={() => onRestore(it.image.id)}
                  className="tbtn"
                  style={{
                    fontSize: 11.5,
                    color: "var(--color-accent)",
                    flex: "none",
                  }}
                >
                  Restore
                </button>
              </div>
            ))
          )}
        </div>

        <div
          style={{
            padding: "13px 16px",
            borderTop: "1px solid var(--color-line)",
          }}
        >
          <button
            onClick={() => {
              if (confirm) {
                onEmpty();
                setConfirm(false);
              } else {
                setConfirm(true);
              }
            }}
            disabled={staged.length === 0 || busy}
            className="tbtn"
            style={{
              width: "100%",
              justifyContent: "center",
              fontWeight: 600,
              background:
                staged.length === 0
                  ? "transparent"
                  : confirm
                    ? "var(--color-reject)"
                    : "var(--color-elev)",
              color: confirm ? "#1a0e0e" : "var(--color-t1)",
              opacity: staged.length === 0 || busy ? 0.5 : 1,
            }}
          >
            {busy
              ? "Emptying…"
              : confirm
                ? `Confirm — delete ${staged.length}`
                : "Empty bin"}
          </button>
          <div
            style={{
              fontSize: 11,
              color: "var(--color-t3)",
              textAlign: "center",
              marginTop: 8,
            }}
          >
            Moves files to OS trash · recoverable from there
          </div>
        </div>
      </div>
    </>
  );
}
