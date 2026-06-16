import { useEffect, useState } from "react";
import { thumbCacheCap, thumbCacheSize, setThumbCacheCap } from "../../lib/ipc";

const GB = 1024 * 1024 * 1024;

function fmtBytes(n: number): string {
  if (n >= GB) return `${(n / GB).toFixed(2)} GB`;
  const mb = n / (1024 * 1024);
  return `${mb.toFixed(0)} MB`;
}

interface SettingsModalProps {
  open: boolean;
  onClose: () => void;
}

export default function SettingsModal({ open, onClose }: SettingsModalProps) {
  const [capGb, setCapGb] = useState("2");
  const [usedBytes, setUsedBytes] = useState<number | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setStatus(null);
    void Promise.all([thumbCacheCap(), thumbCacheSize()])
      .then(([cap, used]) => {
        setCapGb((cap / GB).toFixed(2).replace(/\.?0+$/, ""));
        setUsedBytes(used);
      })
      .catch(() => setStatus("Failed to load settings"));
  }, [open]);

  if (!open) return null;

  const handleSave = async () => {
    const gb = parseFloat(capGb);
    if (!Number.isFinite(gb) || gb <= 0) {
      setStatus("Enter a positive number of GB");
      return;
    }
    setSaving(true);
    setStatus(null);
    try {
      const freed = await setThumbCacheCap(Math.round(gb * GB));
      const used = await thumbCacheSize();
      setUsedBytes(used);
      setStatus(freed > 0 ? `Freed ${fmtBytes(freed)}` : "Saved");
    } catch {
      setStatus("Failed to save");
    } finally {
      setSaving(false);
    }
  };

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
          width: 440,
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
            padding: "14px 18px",
            borderBottom: "1px solid var(--color-line)",
            fontSize: 14,
            fontWeight: 600,
            color: "var(--color-t1)",
          }}
        >
          Settings
        </div>

        <div style={{ padding: "18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Thumbnail cache limit
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Currently using {usedBytes == null ? "…" : fmtBytes(usedBytes)} on
            disk. Oldest thumbnails are evicted when the limit is exceeded.
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <input
              type="number"
              min="0.1"
              step="0.5"
              value={capGb}
              onChange={(e) => setCapGb(e.target.value)}
              style={{
                width: 90,
                background: "var(--color-stage)",
                border: "1px solid var(--color-line-2)",
                borderRadius: "var(--radius-sm)",
                color: "var(--color-t1)",
                padding: "6px 8px",
                fontSize: 13,
                fontFamily: "var(--font-mono)",
                outline: "none",
              }}
            />
            <span style={{ fontSize: 12, color: "var(--color-t2)" }}>GB</span>
            <button
              onClick={() => void handleSave()}
              disabled={saving}
              style={{
                marginLeft: "auto",
                background: "var(--color-accent)",
                color: "#fff",
                border: "none",
                borderRadius: "var(--radius-sm)",
                padding: "6px 14px",
                fontSize: 12,
                cursor: saving ? "default" : "pointer",
                opacity: saving ? 0.6 : 1,
              }}
            >
              {saving ? "Saving…" : "Save"}
            </button>
          </div>
          {status && (
            <div
              style={{ fontSize: 11, color: "var(--color-t3)", marginTop: 10 }}
            >
              {status}
            </div>
          )}
        </div>

        <div
          style={{
            padding: "12px 18px",
            borderTop: "1px solid var(--color-line)",
            display: "flex",
            justifyContent: "flex-end",
          }}
        >
          <button
            onClick={onClose}
            style={{
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              border: "1px solid var(--color-line-2)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
