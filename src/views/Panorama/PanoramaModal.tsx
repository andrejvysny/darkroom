import { useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store/app";
import { panoramaPreview, type PanoramaProjection } from "../../lib/ipc";
import { panoramaErrorMessage, usePanorama } from "../../lib/usePanorama";

const PROJECTIONS: { value: PanoramaProjection; label: string }[] = [
  { value: "auto", label: "Auto" },
  { value: "spherical", label: "Spherical" },
  { value: "cylindrical", label: "Cylindrical" },
  { value: "perspective", label: "Perspective" },
];

/** Debounce for `panoramaPreview` calls on option changes — long enough that a slider drag doesn't
 *  spam re-renders, short enough that the preview still feels responsive. */
const PREVIEW_DEBOUNCE_MS = 350;

const overlay: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,.5)",
  backdropFilter: "blur(2px)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 60,
};

const card: React.CSSProperties = {
  width: 520,
  maxWidth: "92vw",
  maxHeight: "88vh",
  overflowY: "auto",
  background: "#26262a",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-lg)",
  boxShadow: "0 24px 80px rgba(0,0,0,.6)",
};

const fieldLabel: React.CSSProperties = {
  fontSize: 11,
  color: "var(--color-t3)",
  marginBottom: 5,
};

/**
 * Panorama merge modal. Opens when `panoramaSources` (a 2..=10 image-id selection staged by
 * SelectionBar) is non-null. Debounced-previews the merge as options change, then hands off to
 * `usePanorama().startMerge` and closes immediately — the PanoramaPill takes over from there.
 */
export default function PanoramaModal() {
  const sourceIds = useAppStore((s) => s.panoramaSources);
  const setPanoramaSources = useAppStore((s) => s.setPanoramaSources);
  const open_ = sourceIds !== null;
  const { startMerge } = usePanorama();

  const [projection, setProjection] = useState<PanoramaProjection>("auto");
  const [boundaryWarp, setBoundaryWarp] = useState(0);
  const [autoCrop, setAutoCrop] = useState(true);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const previewObjUrl = useRef<string | null>(null);
  const previewSeq = useRef(0);
  const debounceTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Reset transient state each time the modal opens for a fresh source set.
  useEffect(() => {
    if (!open_) return;
    setProjection("auto");
    setBoundaryWarp(0);
    setAutoCrop(true);
    setPreviewError(null);
    setBusy(false);
  }, [open_, sourceIds]);

  // Debounced preview: fires on open + on any option change.
  useEffect(() => {
    if (debounceTimer.current) clearTimeout(debounceTimer.current);
    if (!open_ || !sourceIds || sourceIds.length < 2) return;

    debounceTimer.current = setTimeout(() => {
      const seq = ++previewSeq.current;
      setPreviewLoading(true);
      setPreviewError(null);
      void panoramaPreview({
        imageIds: sourceIds,
        projection,
        boundaryWarp,
        autoCrop,
      })
        .then((url) => {
          if (seq !== previewSeq.current) {
            URL.revokeObjectURL(url);
            return;
          }
          if (previewObjUrl.current) URL.revokeObjectURL(previewObjUrl.current);
          previewObjUrl.current = url;
          setPreviewUrl(url);
          setPreviewLoading(false);
        })
        .catch((err: unknown) => {
          if (seq !== previewSeq.current) return;
          const msg = panoramaErrorMessage(err);
          setPreviewError(msg === "merge engine not available" ? msg : `Preview failed: ${msg}`);
          setPreviewLoading(false);
        });
    }, PREVIEW_DEBOUNCE_MS);

    return () => {
      if (debounceTimer.current) clearTimeout(debounceTimer.current);
    };
  }, [open_, sourceIds, projection, boundaryWarp, autoCrop]);

  // Revoke the last preview object URL when the modal unmounts for good.
  useEffect(() => {
    return () => {
      if (previewObjUrl.current) URL.revokeObjectURL(previewObjUrl.current);
    };
  }, []);

  if (!open_ || !sourceIds) return null;

  const close = () => {
    if (busy) return;
    setPanoramaSources(null);
  };

  const canMerge = sourceIds.length >= 2 && sourceIds.length <= 10 && !busy;

  const handleMerge = () => {
    if (!canMerge) return;
    setBusy(true);
    // Fire-and-forget from the modal's perspective: usePanorama() tracks the job in the store and the
    // PanoramaPill surfaces progress/errors from here on.
    void startMerge({ imageIds: sourceIds, projection, boundaryWarp, autoCrop });
    setPanoramaSources(null);
  };

  return (
    <div
      style={overlay}
      onClick={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div style={card}>
        <div
          style={{
            padding: "14px 18px",
            borderBottom: "1px solid var(--color-line)",
            fontSize: 14,
            fontWeight: 600,
            color: "var(--color-t1)",
          }}
        >
          Merge to Panorama
        </div>

        <div
          style={{
            padding: "16px 18px",
            display: "flex",
            flexDirection: "column",
            gap: 14,
          }}
        >
          <div style={{ fontSize: 12, color: "var(--color-t2)" }}>
            {sourceIds.length} photo{sourceIds.length === 1 ? "" : "s"} selected
          </div>

          {/* Projection segmented control */}
          <div>
            <div style={fieldLabel}>Projection</div>
            <div
              style={{
                display: "flex",
                border: "1px solid var(--color-line-2)",
                borderRadius: "var(--radius-sm)",
                overflow: "hidden",
              }}
            >
              {PROJECTIONS.map((p, i) => (
                <button
                  key={p.value}
                  onClick={() => setProjection(p.value)}
                  style={{
                    flex: 1,
                    padding: "7px 4px",
                    fontSize: 12,
                    border: "none",
                    borderLeft: i > 0 ? "1px solid var(--color-line-2)" : "none",
                    background:
                      projection === p.value ? "var(--color-accent)" : "var(--color-elev)",
                    color: projection === p.value ? "#fff" : "var(--color-t1)",
                    cursor: "pointer",
                  }}
                >
                  {p.label}
                </button>
              ))}
            </div>
          </div>

          {/* Boundary Warp */}
          <div>
            <div style={fieldLabel}>Boundary Warp — {boundaryWarp}</div>
            <input
              type="range"
              min={0}
              max={100}
              value={boundaryWarp}
              onChange={(e) => setBoundaryWarp(Number(e.target.value))}
              style={{ width: "100%" }}
            />
          </div>

          {/* Auto Crop */}
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              fontSize: 12.5,
              color: "var(--color-t1)",
              cursor: "pointer",
            }}
          >
            <input
              type="checkbox"
              checked={autoCrop}
              onChange={(e) => setAutoCrop(e.target.checked)}
            />
            Auto Crop
          </label>

          {/* Preview — 16:6-ish letterbox */}
          <div
            style={{
              position: "relative",
              width: "100%",
              aspectRatio: "16 / 6",
              background: "#000",
              borderRadius: "var(--radius-sm)",
              overflow: "hidden",
              border: "1px solid var(--color-line-2)",
            }}
          >
            {previewUrl && !previewError && (
              <img
                src={previewUrl}
                alt="Panorama preview"
                style={{ width: "100%", height: "100%", objectFit: "contain", display: "block" }}
              />
            )}
            {!previewUrl && !previewLoading && !previewError && (
              <div
                style={{
                  position: "absolute",
                  inset: 0,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  fontSize: 12,
                  color: "var(--color-t3)",
                }}
              >
                Preview will appear here
              </div>
            )}
            {previewLoading && (
              <div
                style={{
                  position: "absolute",
                  inset: 0,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  background: "rgba(0,0,0,.35)",
                }}
              >
                <style>{"@keyframes darkroom-spin{to{transform:rotate(360deg)}}"}</style>
                <div
                  style={{
                    width: 28,
                    height: 28,
                    borderRadius: "50%",
                    border: "3px solid rgba(255,255,255,.22)",
                    borderTopColor: "var(--color-accent)",
                    animation: "darkroom-spin 0.8s linear infinite",
                  }}
                />
              </div>
            )}
            {previewError && !previewLoading && (
              <div
                style={{
                  position: "absolute",
                  inset: 0,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  padding: 16,
                  textAlign: "center",
                  fontSize: 12,
                  color: "var(--color-t2)",
                }}
              >
                {previewError}
              </div>
            )}
          </div>
        </div>

        <div
          style={{
            padding: "12px 18px",
            borderTop: "1px solid var(--color-line)",
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
          }}
        >
          <button
            onClick={close}
            disabled={busy}
            style={{
              border: "1px solid var(--color-line-2)",
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: busy ? "default" : "pointer",
            }}
          >
            Cancel
          </button>
          <button
            onClick={handleMerge}
            disabled={!canMerge}
            style={{
              border: "none",
              background: canMerge ? "var(--color-accent)" : "var(--color-line-2)",
              color: "#fff",
              borderRadius: "var(--radius-sm)",
              padding: "6px 16px",
              fontSize: 12,
              cursor: canMerge ? "pointer" : "default",
              opacity: canMerge ? 1 : 0.6,
            }}
          >
            {busy ? "Merging…" : "Merge"}
          </button>
        </div>
      </div>
    </div>
  );
}
