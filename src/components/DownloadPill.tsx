import { useModelsStore, type DownloadState } from "../store/models";
import { cancelModelDownload } from "../lib/useModelDownloads";
import { ProgressBar } from "./ProgressBar";
import type { ModelGroupId } from "../lib/ipc";

const NAME: Record<ModelGroupId, string> = {
  analysis: "Detection & Scene",
  faces: "People (Faces)",
  mask_ai: "AI Masking",
};

const MB = 1024 * 1024;
function fmtSpeed(b: number): string {
  if (b >= MB) return `${(b / MB).toFixed(1)} MB/s`;
  return `${Math.max(1, Math.round(b / 1024))} KB/s`;
}
function fmtEta(s: number | null): string {
  if (s == null || !isFinite(s)) return "";
  const t = Math.round(s);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}

/**
 * App-wide download indicator: shows whenever any AI model group is downloading, so a long (up to
 * 1.2 GB) pull stays visible + cancellable while the user navigates away from the manager. Reads the
 * shared models store; mounted once in App.
 */
export default function DownloadPill() {
  const downloads = useModelsStore((s) => s.downloads);
  const active = (
    Object.entries(downloads) as [ModelGroupId, DownloadState][]
  ).filter(([, d]) => d && !d.error && d.phase === "downloading");
  if (active.length === 0) return null;

  const [id, dl] = active[0];
  const pct =
    dl.bytesTotal > 0 ? Math.round((dl.bytesDone / dl.bytesTotal) * 100) : 0;
  const meta = [dl.bytesPerSec > 0 && fmtSpeed(dl.bytesPerSec), fmtEta(dl.etaSec)]
    .filter(Boolean)
    .join(" · ");
  const more = active.length - 1;

  return (
    <div
      style={{
        position: "fixed",
        right: 16,
        bottom: 16,
        width: 260,
        zIndex: 65,
        background: "var(--color-elev)",
        border: "1px solid var(--color-line-2)",
        borderRadius: "var(--radius-md)",
        boxShadow: "0 12px 40px rgba(0,0,0,.5)",
        padding: "10px 12px",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <span
          style={{
            flex: 1,
            fontSize: 12,
            color: "var(--color-t1)",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          ↓ {NAME[id]} {pct}%{more > 0 ? ` (+${more})` : ""}
        </span>
        <button
          onClick={() => cancelModelDownload(id)}
          style={{
            fontSize: 11,
            padding: "1px 8px",
            borderRadius: "var(--radius-sm)",
            border: "1px solid var(--color-line-2)",
            background: "transparent",
            color: "var(--color-t2)",
            cursor: "pointer",
          }}
        >
          Stop
        </button>
      </div>
      <div style={{ marginTop: 7 }}>
        <ProgressBar pct={pct} />
      </div>
      {meta && (
        <div style={{ fontSize: 10, color: "var(--color-t3)", marginTop: 5 }}>
          {meta}
        </div>
      )}
    </div>
  );
}
