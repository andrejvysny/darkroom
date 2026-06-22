import { useEffect, useRef, useState } from "react";
import {
  thumbCacheCap,
  thumbCacheSize,
  setThumbCacheCap,
  analysisDetectorSize,
  setAnalysisDetectorSize,
  previewEdge,
  updatePreviewEdge,
  appLibraryRoot,
  setLibraryRoot,
  featuresBackfill,
  databaseReset,
  sidecarsWriteAll,
  sidecarsRebuild,
  facesDeleteAll,
} from "../../lib/ipc";
import { pickFolder } from "../../lib/importFlow";

const GB = 1024 * 1024 * 1024;

function fmtBytes(n: number): string {
  if (n >= GB) return `${(n / GB).toFixed(2)} GB`;
  return `${(n / (1024 * 1024)).toFixed(0)} MB`;
}

interface SettingsModalProps {
  open: boolean;
  onClose: () => void;
}

const sectionStyle: React.CSSProperties = {
  padding: "18px 20px",
  borderBottom: "1px solid var(--color-line)",
};

const labelStyle: React.CSSProperties = {
  fontSize: 13,
  fontWeight: 500,
  color: "var(--color-t1)",
  marginBottom: 4,
};

const descStyle: React.CSSProperties = {
  fontSize: 11,
  color: "var(--color-t3)",
  marginBottom: 10,
  lineHeight: 1.5,
};

const btnBase: React.CSSProperties = {
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-sm)",
  padding: "6px 14px",
  fontSize: 12,
  cursor: "pointer",
};

const btnSecondary: React.CSSProperties = {
  ...btnBase,
  background: "var(--color-elev)",
  color: "var(--color-t1)",
};

const btnAccent: React.CSSProperties = {
  ...btnBase,
  background: "var(--color-accent)",
  color: "#fff",
  border: "none",
};

const segmentBtn = (active: boolean): React.CSSProperties => ({
  flex: 1,
  ...btnBase,
  background: active ? "var(--color-accent)" : "var(--color-elev)",
  color: active ? "#fff" : "var(--color-t1)",
  textAlign: "center",
  whiteSpace: "nowrap",
});

export default function SettingsModal({ open, onClose }: SettingsModalProps) {
  const [capGb, setCapGb] = useState("2");
  const [usedBytes, setUsedBytes] = useState<number | null>(null);
  const [libRoot, setLibRoot] = useState<string | null>(null);
  const [pickingRoot, setPickingRoot] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [mdSize, setMdSize] = useState(1280);
  const [pEdge, setPEdge] = useState(0);
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [backfilling, setBackfilling] = useState(false);
  const [sidecarBusy, setSidecarBusy] = useState(false);
  const [confirmFaceWipe, setConfirmFaceWipe] = useState(false);
  const [faceWiping, setFaceWiping] = useState(false);

  // Track whether the initial load has settled so debounce doesn't fire on open
  const initializedRef = useRef(false);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const statusTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const showStatus = (msg: string) => {
    setStatus(msg);
    if (statusTimerRef.current) clearTimeout(statusTimerRef.current);
    statusTimerRef.current = setTimeout(() => setStatus(null), 3000);
  };

  useEffect(() => {
    if (!open) return;
    initializedRef.current = false;
    setStatus(null);
    setConfirmReset(false);
    setConfirmFaceWipe(false);
    void Promise.all([
      thumbCacheCap(),
      thumbCacheSize(),
      analysisDetectorSize(),
      appLibraryRoot(),
      previewEdge(),
    ])
      .then(([cap, used, size, root, pe]) => {
        setCapGb((cap / GB).toFixed(2).replace(/\.?0+$/, ""));
        setUsedBytes(used);
        setMdSize(size);
        setLibRoot(root);
        setPEdge(pe);
        initializedRef.current = true;
      })
      .catch(() => showStatus("Failed to load settings"));
  }, [open]);

  // Debounced auto-save for cache cap
  useEffect(() => {
    if (!initializedRef.current) return;
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      const gb = parseFloat(capGb);
      if (!Number.isFinite(gb) || gb <= 0) return;
      void setThumbCacheCap(Math.round(gb * GB))
        .then(() => thumbCacheSize())
        .then((used) => {
          setUsedBytes(used);
          showStatus("Cache limit saved");
        })
        .catch(() => showStatus("Failed to save cache limit"));
    }, 700);
    return () => {
      if (debounceRef.current) clearTimeout(debounceRef.current);
    };
  }, [capGb]);

  const handlePreviewEdge = (edge: number) => {
    setPEdge(edge);
    void updatePreviewEdge(edge)
      .then((applied) => {
        setPEdge(applied);
        showStatus(`Preview resolution set to ${applied}px — regenerating`);
      })
      .catch(() => showStatus("Failed to save preview resolution"));
  };

  const handleChangeLibraryRoot = async () => {
    const picked = await pickFolder("Select library location");
    if (!picked) return;
    setPickingRoot(true);
    try {
      await setLibraryRoot(picked);
      setLibRoot(picked);
      showStatus("Library location saved");
    } catch {
      showStatus("Failed to set library location");
    } finally {
      setPickingRoot(false);
    }
  };

  const handleMdSize = (size: number) => {
    setMdSize(size);
    void setAnalysisDetectorSize(size)
      .then(() => showStatus(`Animal detection set to ${size}px`))
      .catch(() => showStatus("Failed to save resolution"));
  };

  const handleBackfill = () => {
    setBackfilling(true);
    void featuresBackfill()
      .then((n) => showStatus(`Computed features for ${n} image(s)`))
      .catch(() => showStatus("Failed to compute features"))
      .finally(() => setBackfilling(false));
  };

  const handleWriteSidecars = () => {
    setSidecarBusy(true);
    void sidecarsWriteAll()
      .then((n) => showStatus(`Wrote ${n} sidecar file(s)`))
      .catch(() => showStatus("Failed to write sidecars"))
      .finally(() => setSidecarBusy(false));
  };

  const handleRebuildSidecars = () => {
    setSidecarBusy(true);
    void sidecarsRebuild()
      .then((n) => showStatus(`Restored ${n} image(s) from sidecars`))
      .catch(() => showStatus("Failed to rebuild from sidecars"))
      .finally(() => setSidecarBusy(false));
  };

  const handleReset = async () => {
    if (!confirmReset) {
      setConfirmReset(true);
      return;
    }
    setResetting(true);
    try {
      await databaseReset();
      window.location.reload();
    } catch {
      showStatus("Reset failed");
      setResetting(false);
      setConfirmReset(false);
    }
  };

  if (!open) return null;

  return (
    <div
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,.5)",
        backdropFilter: "blur(3px)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 50,
      }}
    >
      <div
        style={{
          width: 580,
          maxWidth: "94vw",
          maxHeight: "88vh",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.7)",
          display: "flex",
          flexDirection: "column",
          overflow: "hidden",
        }}
      >
        {/* Header */}
        <div
          style={{
            padding: "12px 16px 12px 20px",
            borderBottom: "1px solid var(--color-line)",
            display: "flex",
            alignItems: "center",
            flexShrink: 0,
            gap: 10,
          }}
        >
          <span
            style={{
              fontSize: 14,
              fontWeight: 600,
              color: "var(--color-t1)",
              flex: "0 0 auto",
            }}
          >
            Settings
          </span>
          <span
            style={{
              flex: 1,
              fontSize: 11,
              color: "var(--color-t3)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              opacity: status ? 1 : 0,
              transition: "opacity 0.2s",
            }}
          >
            {status ?? ""}
          </span>
          <button
            onClick={onClose}
            title="Close"
            style={{
              flex: "0 0 auto",
              background: "none",
              border: "none",
              color: "var(--color-t3)",
              fontSize: 18,
              lineHeight: 1,
              cursor: "pointer",
              padding: "2px 4px",
              borderRadius: "var(--radius-sm)",
            }}
          >
            ✕
          </button>
        </div>

        {/* Scrollable body */}
        <div style={{ overflowY: "auto", flex: 1 }}>
          {/* Library location */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Library location</div>
            <div style={descStyle}>
              Where copy/move imports file photos (under{" "}
              <span style={{ fontFamily: "var(--font-mono)" }}>
                YYYY/YYYY-MM-DD
              </span>
              ). Existing photos stay put; applies to new imports.
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <div
                title={libRoot ?? undefined}
                style={{
                  flex: 1,
                  minWidth: 0,
                  background: "var(--color-stage)",
                  border: "1px solid var(--color-line-2)",
                  borderRadius: "var(--radius-sm)",
                  color: libRoot ? "var(--color-t1)" : "var(--color-t3)",
                  padding: "6px 8px",
                  fontSize: 12,
                  fontFamily: "var(--font-mono)",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {libRoot ?? "Not set — choose a folder"}
              </div>
              <button
                onClick={() => void handleChangeLibraryRoot()}
                disabled={pickingRoot}
                style={{
                  ...btnAccent,
                  opacity: pickingRoot ? 0.6 : 1,
                  cursor: pickingRoot ? "default" : "pointer",
                }}
              >
                Change…
              </button>
            </div>
          </div>

          {/* Thumbnail cache */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Thumbnail cache</div>
            <div style={descStyle}>
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
                  width: 80,
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
              <span style={{ fontSize: 12, color: "var(--color-t2)" }}>
                GB limit
              </span>
            </div>
          </div>

          {/* Animal detection resolution */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Animal detection resolution</div>
            <div style={descStyle}>
              MegaDetector input size. Higher = better recall on small/distant
              animals but slower. Re-analyze to apply.
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              {(
                [
                  [1280, "1280px — best recall"],
                  [640, "640px — faster"],
                ] as [number, string][]
              ).map(([size, label]) => (
                <button
                  key={size}
                  onClick={() => handleMdSize(size)}
                  style={segmentBtn(mdSize === size)}
                >
                  {label}
                </button>
              ))}
            </div>
          </div>

          {/* Preview resolution */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Preview resolution</div>
            <div style={descStyle}>
              Longest edge of the sharp full-screen preview per photo (defaults
              to your display). Higher = crisper when viewing large, but more
              disk. Changes regenerate previews in the background.
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              {([2560, 3200, 3840, 4096] as number[]).map((edge) => (
                <button
                  key={edge}
                  onClick={() => handlePreviewEdge(edge)}
                  style={segmentBtn(pEdge === edge)}
                >
                  {edge}
                </button>
              ))}
            </div>
          </div>

          {/* Compute features */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Compute image features</div>
            <div style={descStyle}>
              Analyzes each photo's lighting/sharpness (as-shot white balance,
              histograms, focus) for future AI assistance. Runs in the
              background; safe to leave.
            </div>
            <button
              onClick={handleBackfill}
              disabled={backfilling}
              style={{
                ...btnSecondary,
                opacity: backfilling ? 0.6 : 1,
                cursor: backfilling ? "default" : "pointer",
              }}
            >
              {backfilling ? "Computing…" : "Compute features"}
            </button>
          </div>

          {/* Edit backups */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Edit backups (sidecars)</div>
            <div style={descStyle}>
              Edits, ratings, and keywords are written to a small{" "}
              <code>.json</code> file next to each RAW, so the catalog can be
              rebuilt if lost.
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <button
                onClick={handleWriteSidecars}
                disabled={sidecarBusy}
                style={{
                  ...btnSecondary,
                  opacity: sidecarBusy ? 0.6 : 1,
                  cursor: sidecarBusy ? "default" : "pointer",
                }}
              >
                {sidecarBusy ? "Working…" : "Write all sidecars"}
              </button>
              <button
                onClick={handleRebuildSidecars}
                disabled={sidecarBusy}
                style={{
                  ...btnSecondary,
                  opacity: sidecarBusy ? 0.6 : 1,
                  cursor: sidecarBusy ? "default" : "pointer",
                }}
              >
                {sidecarBusy ? "Working…" : "Rebuild from sidecars"}
              </button>
            </div>
          </div>

          {/* Danger zone label */}
          <div
            style={{
              padding: "14px 20px 6px",
              borderBottom: "1px solid var(--color-line)",
            }}
          >
            <div
              style={{
                fontSize: 10,
                fontWeight: 600,
                letterSpacing: "0.08em",
                color: "var(--color-t3)",
                textTransform: "uppercase",
              }}
            >
              Danger zone
            </div>
          </div>

          {/* Face data */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Face data</div>
            <div style={descStyle}>
              Face grouping runs entirely on this Mac; face data is stored only
              in your local catalog and never leaves this device. Deletes all
              detected faces, embeddings, and people (your photos are
              untouched).
            </div>
            <button
              onClick={async () => {
                if (!confirmFaceWipe) {
                  setConfirmFaceWipe(true);
                  return;
                }
                setFaceWiping(true);
                try {
                  await facesDeleteAll();
                  showStatus("Face data deleted");
                } catch (e) {
                  showStatus(`Delete failed: ${e}`);
                } finally {
                  setFaceWiping(false);
                  setConfirmFaceWipe(false);
                }
              }}
              disabled={faceWiping}
              style={{
                ...btnBase,
                background: confirmFaceWipe ? "#b3261e" : "var(--color-elev)",
                color: confirmFaceWipe
                  ? "#fff"
                  : "var(--color-danger, #e5685f)",
                borderColor: confirmFaceWipe
                  ? "#b3261e"
                  : "var(--color-line-2)",
                opacity: faceWiping ? 0.6 : 1,
                cursor: faceWiping ? "default" : "pointer",
              }}
            >
              {faceWiping
                ? "Deleting…"
                : confirmFaceWipe
                  ? "Click again to confirm delete"
                  : "Delete all face data…"}
            </button>
          </div>

          {/* Reset catalog */}
          <div style={{ ...sectionStyle, borderBottom: "none" }}>
            <div style={labelStyle}>Reset catalog</div>
            <div style={descStyle}>
              Wipes the database (index, metadata, ratings, keywords, settings,
              imported folders) and the thumbnail cache, leaving the app empty.
              Your photo files on disk are never touched — re-import to
              repopulate.
            </div>
            <button
              onClick={() => void handleReset()}
              disabled={resetting}
              style={{
                ...btnBase,
                background: confirmReset ? "#b3261e" : "var(--color-elev)",
                color: confirmReset ? "#fff" : "var(--color-danger, #e5685f)",
                borderColor: confirmReset ? "#b3261e" : "var(--color-line-2)",
                opacity: resetting ? 0.6 : 1,
                cursor: resetting ? "default" : "pointer",
              }}
            >
              {resetting
                ? "Resetting…"
                : confirmReset
                  ? "Click again to confirm wipe"
                  : "Reset catalog…"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
