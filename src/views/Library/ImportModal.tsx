import { useEffect, useState } from "react";
import Icon, { IconName } from "../../components/Icon";
import type { ImportMode } from "../../lib/ipc";

interface Props {
  open: boolean;
  onClose: () => void;
  onChoose: (mode: ImportMode, recursive: boolean) => void;
}

const MODES: {
  mode: ImportMode;
  icon: IconName;
  title: string;
  desc: string;
}[] = [
  {
    mode: "copy",
    icon: "import",
    title: "Copy + add",
    desc: "Copy files into the library (routed by date), verify by hash, leave the source untouched. Best for cards.",
  },
  {
    mode: "move",
    icon: "export",
    title: "Move + add",
    desc: "Copy and verify, then send the originals to Trash only after a hash match.",
  },
  {
    mode: "reference",
    icon: "folder",
    title: "Reference in place",
    desc: "Catalog files where they already live, without copying. The source folder becomes watched.",
  },
];

export default function ImportModal({ open, onClose, onChoose }: Props) {
  const [recursive, setRecursive] = useState(true);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [open, onClose]);

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
        paddingTop: "16vh",
        zIndex: 50,
      }}
    >
      <div
        style={{
          width: 460,
          maxWidth: "92vw",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.6)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            padding: "14px 16px",
            borderBottom: "1px solid var(--color-line)",
            fontWeight: 600,
            fontSize: 13.5,
            color: "var(--color-t1)",
          }}
        >
          Import photos
        </div>
        <div style={{ padding: 10 }}>
          {MODES.map((m) => (
            <button
              key={m.mode}
              onClick={() => onChoose(m.mode, recursive)}
              style={{
                display: "flex",
                gap: 12,
                width: "100%",
                textAlign: "left",
                alignItems: "flex-start",
                padding: "11px 12px",
                borderRadius: "var(--radius-md)",
                border: "1px solid var(--color-line)",
                background: "transparent",
                color: "var(--color-t1)",
                cursor: "pointer",
                marginBottom: 8,
              }}
            >
              <Icon
                name={m.icon}
                size={18}
                style={
                  {
                    color: "var(--color-accent)",
                    marginTop: 1,
                  } as React.CSSProperties
                }
              />
              <span style={{ display: "block" }}>
                <span
                  style={{ display: "block", fontSize: 13, fontWeight: 600 }}
                >
                  {m.title}
                </span>
                <span
                  style={{
                    display: "block",
                    fontSize: 11.5,
                    color: "var(--color-t3)",
                    lineHeight: 1.45,
                    marginTop: 2,
                  }}
                >
                  {m.desc}
                </span>
              </span>
            </button>
          ))}
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "4px 4px 2px",
              fontSize: 12,
              color: "var(--color-t2)",
              cursor: "pointer",
              userSelect: "none",
            }}
          >
            <input
              type="checkbox"
              checked={recursive}
              onChange={(e) => setRecursive(e.target.checked)}
              style={{ accentColor: "var(--color-accent)", cursor: "pointer" }}
            />
            Include subfolders (recursive)
          </label>
        </div>
      </div>
    </div>
  );
}
