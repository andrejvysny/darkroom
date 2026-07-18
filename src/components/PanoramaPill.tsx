import { useModelsStore, type DownloadState } from "../store/models";
import { panoramaPhaseLabel, usePanorama } from "../lib/usePanorama";
import type { ModelGroupId } from "../lib/ipc";

/**
 * App-wide panorama-merge indicator, cloned from DownloadPill's visual language: a floating pill with
 * an indeterminate progress shimmer (the backend doesn't report fractional progress, only a phase)
 * and a Stop button. Reads the shared `panoramaJob` store field; mounted once in App, alongside
 * DownloadPill (offset above it when a model download pill is also showing).
 */
export default function PanoramaPill() {
  const { job, cancelMerge } = usePanorama();

  const hasDownloadPill = useModelsStore((s) =>
    (Object.entries(s.downloads) as [ModelGroupId, DownloadState][]).some(
      ([, d]) => d && !d.error && d.phase === "downloading",
    ),
  );

  if (!job) return null;

  return (
    <div
      style={{
        position: "fixed",
        right: 16,
        bottom: hasDownloadPill ? 92 : 16,
        width: 260,
        zIndex: 65,
        background: "var(--color-elev)",
        border: "1px solid var(--color-line-2)",
        borderRadius: "var(--radius-md)",
        boxShadow: "0 12px 40px rgba(0,0,0,.5)",
        padding: "10px 12px",
      }}
    >
      <style>
        {
          "@keyframes darkroom-panorama-shimmer{0%{transform:translateX(-100%)}100%{transform:translateX(350%)}}"
        }
      </style>
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
          Merging panorama — {panoramaPhaseLabel(job.phase)}
        </span>
        <button
          onClick={() => void cancelMerge()}
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
      <div
        style={{
          marginTop: 7,
          height: 3,
          borderRadius: 2,
          background: "var(--color-line)",
          overflow: "hidden",
          position: "relative",
        }}
      >
        <div
          style={{
            position: "absolute",
            top: 0,
            bottom: 0,
            width: "40%",
            borderRadius: 2,
            background: "var(--color-accent)",
            animation: "darkroom-panorama-shimmer 1.1s ease-in-out infinite",
          }}
        />
      </div>
    </div>
  );
}
