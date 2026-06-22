import { useEffect, useState } from "react";
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
  const [libRoot, setLibRoot] = useState<string | null>(null);
  const [pickingRoot, setPickingRoot] = useState(false);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [mdSize, setMdSize] = useState(1280);
  const [pEdge, setPEdge] = useState(0);
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [backfilling, setBackfilling] = useState(false);
  const [sidecarBusy, setSidecarBusy] = useState(false);
  const [confirmFaceWipe, setConfirmFaceWipe] = useState(false);
  const [faceWiping, setFaceWiping] = useState(false);

  useEffect(() => {
    if (!open) return;
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
      })
      .catch(() => setStatus("Failed to load settings"));
  }, [open]);

  const handlePreviewEdge = (edge: number) => {
    setPEdge(edge);
    void updatePreviewEdge(edge)
      .then((applied) => {
        setPEdge(applied);
        setStatus(
          `Preview resolution set to ${applied}px — regenerating previews`,
        );
      })
      .catch(() => setStatus("Failed to save preview resolution"));
  };

  const handleChangeLibraryRoot = async () => {
    const picked = await pickFolder("Select library location");
    if (!picked) return;
    setPickingRoot(true);
    setStatus(null);
    try {
      await setLibraryRoot(picked);
      setLibRoot(picked);
      setStatus("Library location saved — applies to new copy/move imports");
    } catch {
      setStatus("Failed to set library location");
    } finally {
      setPickingRoot(false);
    }
  };

  const handleMdSize = (size: number) => {
    setMdSize(size);
    void setAnalysisDetectorSize(size)
      .then(() =>
        setStatus(`Animal detection set to ${size}px — re-analyze to apply`),
      )
      .catch(() => setStatus("Failed to save resolution"));
  };

  const handleBackfill = () => {
    setBackfilling(true);
    setStatus(null);
    void featuresBackfill()
      .then((n) => setStatus(`Computed features for ${n} image(s)`))
      .catch(() => setStatus("Failed to compute features"))
      .finally(() => setBackfilling(false));
  };

  const handleWriteSidecars = () => {
    setSidecarBusy(true);
    setStatus(null);
    void sidecarsWriteAll()
      .then((n) => setStatus(`Wrote ${n} sidecar file(s) next to your RAWs`))
      .catch(() => setStatus("Failed to write sidecars"))
      .finally(() => setSidecarBusy(false));
  };

  const handleRebuildSidecars = () => {
    setSidecarBusy(true);
    setStatus(null);
    void sidecarsRebuild()
      .then((n) => setStatus(`Restored ${n} image(s) from sidecars`))
      .catch(() => setStatus("Failed to rebuild from sidecars"))
      .finally(() => setSidecarBusy(false));
  };

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

  const handleReset = async () => {
    if (!confirmReset) {
      setConfirmReset(true);
      return;
    }
    setResetting(true);
    setStatus(null);
    try {
      await databaseReset();
      // Catalog wiped to empty; reload to re-bootstrap the UI with the now-empty state.
      window.location.reload();
    } catch {
      setStatus("Reset failed");
      setResetting(false);
      setConfirmReset(false);
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

        <div style={{ padding: "18px 18px 0" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Library location
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Where copy/move imports file photos (under{" "}
            <span style={{ fontFamily: "var(--font-mono)" }}>
              YYYY/YYYY-MM-DD
            </span>
            ). Existing photos stay put; this applies to new imports.
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
                background: "var(--color-accent)",
                color: "#fff",
                border: "none",
                borderRadius: "var(--radius-sm)",
                padding: "6px 14px",
                fontSize: 12,
                cursor: pickingRoot ? "default" : "pointer",
                opacity: pickingRoot ? 0.6 : 1,
                whiteSpace: "nowrap",
              }}
            >
              Change…
            </button>
          </div>
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

        <div style={{ padding: "0 18px 18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Animal detection resolution
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            MegaDetector input size. Higher = better recall on small/distant
            animals but slower. Re-analyze to apply.
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            {[
              [1280, "1280px — best recall"],
              [640, "640px — faster"],
            ].map(([size, label]) => (
              <button
                key={size}
                onClick={() => handleMdSize(size as number)}
                style={{
                  flex: 1,
                  background:
                    mdSize === size
                      ? "var(--color-accent)"
                      : "var(--color-elev)",
                  color: mdSize === size ? "#fff" : "var(--color-t1)",
                  border: "1px solid var(--color-line-2)",
                  borderRadius: "var(--radius-sm)",
                  padding: "6px 10px",
                  fontSize: 12,
                  cursor: "pointer",
                }}
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        <div style={{ padding: "0 18px 18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Preview resolution
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Longest edge of the sharp full-screen preview generated per photo
            (defaults to your display). Higher = crisper when viewing large, but
            more disk. Changing it regenerates previews in the background.
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            {[
              [2560, "2560"],
              [3200, "3200"],
              [3840, "3840"],
              [4096, "4096"],
            ].map(([edge, label]) => (
              <button
                key={edge}
                onClick={() => handlePreviewEdge(edge as number)}
                style={{
                  flex: 1,
                  background:
                    pEdge === edge
                      ? "var(--color-accent)"
                      : "var(--color-elev)",
                  color: pEdge === edge ? "#fff" : "var(--color-t1)",
                  border: "1px solid var(--color-line-2)",
                  borderRadius: "var(--radius-sm)",
                  padding: "6px 10px",
                  fontSize: 12,
                  cursor: "pointer",
                }}
              >
                {label}
              </button>
            ))}
          </div>
        </div>

        <div style={{ padding: "0 18px 18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Compute image features
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Analyzes each photo's lighting/sharpness (as-shot white balance,
            histograms, focus) for future AI assistance. Runs in the background;
            safe to leave.
          </div>
          <button
            onClick={handleBackfill}
            disabled={backfilling}
            style={{
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              border: "1px solid var(--color-line-2)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: backfilling ? "default" : "pointer",
              opacity: backfilling ? 0.6 : 1,
            }}
          >
            {backfilling ? "Computing…" : "Compute features"}
          </button>
        </div>

        <div style={{ padding: "0 18px 18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Edit backups (sidecars)
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Edits, ratings, and keywords are written to a small{" "}
            <code>.json</code> file next to each RAW, so the catalog can be
            rebuilt if lost. Write them for your whole library, or restore the
            catalog from them.
          </div>
          <div style={{ display: "flex", gap: 8 }}>
            <button
              onClick={handleWriteSidecars}
              disabled={sidecarBusy}
              style={{
                background: "var(--color-elev)",
                color: "var(--color-t1)",
                border: "1px solid var(--color-line-2)",
                borderRadius: "var(--radius-sm)",
                padding: "6px 14px",
                fontSize: 12,
                cursor: sidecarBusy ? "default" : "pointer",
                opacity: sidecarBusy ? 0.6 : 1,
              }}
            >
              {sidecarBusy ? "Working…" : "Write all sidecars"}
            </button>
            <button
              onClick={handleRebuildSidecars}
              disabled={sidecarBusy}
              style={{
                background: "var(--color-elev)",
                color: "var(--color-t1)",
                border: "1px solid var(--color-line-2)",
                borderRadius: "var(--radius-sm)",
                padding: "6px 14px",
                fontSize: 12,
                cursor: sidecarBusy ? "default" : "pointer",
                opacity: sidecarBusy ? 0.6 : 1,
              }}
            >
              {sidecarBusy ? "Working…" : "Rebuild from sidecars"}
            </button>
          </div>
        </div>

        <div style={{ padding: "0 18px 18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Face data
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Face grouping runs entirely on this Mac; face data is stored only in
            your local catalog and never leaves this device. Delete all detected
            faces, embeddings, and people (your photos are untouched).
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
                setStatus("Face data deleted");
              } catch (e) {
                setStatus(`Delete failed: ${e}`);
              } finally {
                setFaceWiping(false);
                setConfirmFaceWipe(false);
              }
            }}
            disabled={faceWiping}
            style={{
              background: confirmFaceWipe ? "#b3261e" : "var(--color-elev)",
              color: confirmFaceWipe ? "#fff" : "var(--color-danger, #e5685f)",
              border: `1px solid ${confirmFaceWipe ? "#b3261e" : "var(--color-line-2)"}`,
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: faceWiping ? "default" : "pointer",
              opacity: faceWiping ? 0.6 : 1,
            }}
          >
            {faceWiping
              ? "Deleting…"
              : confirmFaceWipe
                ? "Click again to confirm delete"
                : "Delete all face data…"}
          </button>
        </div>

        <div style={{ padding: "0 18px 18px" }}>
          <div
            style={{ fontSize: 13, color: "var(--color-t1)", marginBottom: 4 }}
          >
            Reset catalog
          </div>
          <div
            style={{ fontSize: 11, color: "var(--color-t3)", marginBottom: 10 }}
          >
            Wipes the database (index, metadata, ratings, keywords, settings,
            imported folders) and the thumbnail cache, leaving the app empty.
            Your photo files on disk are never touched — re-import to
            repopulate.
          </div>
          <button
            onClick={() => void handleReset()}
            disabled={resetting}
            style={{
              background: confirmReset ? "#b3261e" : "var(--color-elev)",
              color: confirmReset ? "#fff" : "var(--color-danger, #e5685f)",
              border: `1px solid ${confirmReset ? "#b3261e" : "var(--color-line-2)"}`,
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: resetting ? "default" : "pointer",
              opacity: resetting ? 0.6 : 1,
            }}
          >
            {resetting
              ? "Resetting…"
              : confirmReset
                ? "Click again to confirm wipe"
                : "Reset catalog…"}
          </button>
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
