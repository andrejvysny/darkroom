import { useEffect, useRef, useState } from "react";
import { save } from "@tauri-apps/plugin-dialog";
import { getVersion } from "@tauri-apps/api/app";
import {
  checkForUpdate,
  autoCheckEnabled,
  setAutoCheckEnabled,
} from "../../lib/useUpdater";
import {
  thumbCacheCap,
  thumbCacheSize,
  setThumbCacheCap,
  previewEdge,
  updatePreviewEdge,
  appLibraryRoot,
  setLibraryRoot,
  featuresBackfill,
  databaseReset,
  sidecarsWriteAll,
  sidecarsRebuild,
  facesDeleteAll,
  logsStatus,
  setLogsDirectory,
  setLogLevel,
  logsExportZip,
  logsDeleteAll,
  type LogsStatus,
  catalogBackupNow,
  catalogBackupStatus,
  type BackupStatus,
} from "../../lib/ipc";
import { pickFolder } from "../../lib/importFlow";
import { useAppStore } from "../../store/app";
import { useSuggest } from "../../lib/useSuggest";

const GB = 1024 * 1024 * 1024;

function fmtBytes(n: number): string {
  if (n >= GB) return `${(n / GB).toFixed(2)} GB`;
  return `${(n / (1024 * 1024)).toFixed(0)} MB`;
}

const pct = (v: number | null): string =>
  v == null ? "—" : `${(v * 100).toFixed(1)}%`;

function fmtAgo(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
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
  const [pEdge, setPEdge] = useState(0);
  const setModelManagerOpen = useAppStore((s) => s.setModelManagerOpen);
  const [confirmReset, setConfirmReset] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [backfilling, setBackfilling] = useState(false);
  const [sidecarBusy, setSidecarBusy] = useState(false);
  const [confirmFaceWipe, setConfirmFaceWipe] = useState(false);
  const [faceWiping, setFaceWiping] = useState(false);
  const [logs, setLogs] = useState<LogsStatus | null>(null);
  const [logsBusy, setLogsBusy] = useState(false);
  const [confirmLogsDelete, setConfirmLogsDelete] = useState(false);
  const [backup, setBackup] = useState<BackupStatus | null>(null);
  const [backupBusy, setBackupBusy] = useState(false);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [autoCheck, setAutoCheck] = useState(autoCheckEnabled());
  const suggest = useSuggest();
  const showSuggestions = useAppStore((s) => s.showSuggestions);
  const setShowSuggestions = useAppStore((s) => s.setShowSuggestions);

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
    setConfirmLogsDelete(false);
    setAutoCheck(autoCheckEnabled());
    void getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(null));
    // Cheap catalog aggregate; re-read on open so the census reflects culling done since mount.
    void suggest.reload();
    void Promise.all([
      thumbCacheCap(),
      thumbCacheSize(),
      appLibraryRoot(),
      previewEdge(),
      logsStatus(),
      catalogBackupStatus(),
    ])
      .then(([cap, used, root, pe, logStatus, backupStatus]) => {
        setCapGb((cap / GB).toFixed(2).replace(/\.?0+$/, ""));
        setUsedBytes(used);
        setLibRoot(root);
        setPEdge(pe);
        setLogs(logStatus);
        setBackup(backupStatus);
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

  const handleBackfill = () => {
    setBackfilling(true);
    void featuresBackfill()
      .then((n) => showStatus(`Computed features for ${n} image(s)`))
      .catch(() => showStatus("Failed to compute features"))
      .finally(() => setBackfilling(false));
  };

  const handleLogsLocation = async () => {
    const picked = await pickFolder("Select logs location");
    if (!picked) return;
    setLogsBusy(true);
    try {
      setLogs(await setLogsDirectory(picked));
      showStatus("Logs location changed");
    } catch {
      showStatus("Failed to change logs location");
    } finally {
      setLogsBusy(false);
    }
  };

  const handleLogLevel = (level: LogsStatus["level"]) => {
    setLogs((prev) => (prev ? { ...prev, level } : prev));
    void setLogLevel(level)
      .then((next) => {
        setLogs(next);
        showStatus(`Log level set to ${level}`);
      })
      .catch(() => showStatus("Failed to set log level"));
  };

  const handleExportLogs = async () => {
    const stamp = new Date().toISOString().replace(/[:.]/g, "-").slice(0, 19);
    const dest = await save({
      defaultPath: `darkroom-logs-${stamp}.zip`,
      filters: [{ name: "ZIP", extensions: ["zip"] }],
    });
    if (!dest) return;
    setLogsBusy(true);
    try {
      const bytes = await logsExportZip(dest);
      showStatus(`Exported logs (${fmtBytes(bytes)})`);
    } catch {
      showStatus("Failed to export logs");
    } finally {
      setLogsBusy(false);
    }
  };

  const handleDeleteLogs = async () => {
    if (!confirmLogsDelete) {
      setConfirmLogsDelete(true);
      return;
    }
    setLogsBusy(true);
    try {
      setLogs(await logsDeleteAll());
      showStatus("Logs deleted");
    } catch {
      showStatus("Failed to delete logs");
    } finally {
      setLogsBusy(false);
      setConfirmLogsDelete(false);
    }
  };

  const handleBackupNow = () => {
    setBackupBusy(true);
    void catalogBackupNow()
      .then((next) => {
        setBackup(next);
        showStatus("Backup complete");
      })
      .catch(() => showStatus("Backup failed"))
      .finally(() => setBackupBusy(false));
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

          {/* AI models */}
          <div style={sectionStyle}>
            <div style={labelStyle}>AI &amp; Models</div>
            <div style={descStyle}>
              Manage on-device AI models — object &amp; scene detection, People
              (faces), and AI object-select masks. Download or remove models,
              watch download progress, pick the animal-detection resolution, face
              stage, and AI-mask quality tier.
            </div>
            <button
              style={btnSecondary}
              onClick={() => {
                onClose();
                setModelManagerOpen(true);
              }}
            >
              Manage models…
            </button>
          </div>

          {/* Updates */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Updates</div>
            <div style={descStyle}>
              Darkroom updates itself from GitHub Releases. You're on version{" "}
              <span style={{ fontFamily: "var(--font-mono)" }}>
                {appVersion ?? "—"}
              </span>
              . When an update is found you're prompted to download and restart.
            </div>
            <label
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                fontSize: 12,
                color: "var(--color-t2)",
                marginBottom: 10,
                cursor: "pointer",
              }}
            >
              <input
                type="checkbox"
                checked={autoCheck}
                onChange={(e) => {
                  setAutoCheck(e.target.checked);
                  setAutoCheckEnabled(e.target.checked);
                }}
              />
              Automatically check for updates on launch
            </label>
            <button
              style={btnSecondary}
              onClick={() => {
                onClose();
                void checkForUpdate({ silent: false });
              }}
            >
              Check for updates
            </button>
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

          {/* Pick suggestions */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Pick suggestions</div>
            <div style={descStyle}>
              Learns which photos you keep from your own picks and rejects, and
              badges likely keepers in the grid with a hollow ring. It never
              flags anything for you — accepting a suggestion is always a
              keypress. A small slice of photos is deliberately left un-badged
              so the accuracy below stays honest.
            </div>
            {suggest.status && (
              <div
                style={{
                  fontSize: 11.5,
                  color: "var(--color-t2)",
                  lineHeight: 1.7,
                  marginBottom: 10,
                }}
              >
                {suggest.status.modelId == null ? (
                  <div>Not trained yet — no photos are badged.</div>
                ) : (
                  <>
                    <div>
                      Trained{" "}
                      {suggest.status.trainedAt == null
                        ? "—"
                        : fmtAgo(suggest.status.trainedAt)}{" "}
                      on {suggest.status.trainedPos ?? 0} picks +{" "}
                      {suggest.status.trainedNeg ?? 0} rejects
                    </div>
                    <div>
                      Accuracy {pct(suggest.status.cvAuc)} · Precision{" "}
                      {pct(suggest.status.cvAuprc)} · Best of burst{" "}
                      {pct(suggest.status.top1Agreement)}
                    </div>
                    <div>
                      Scored {suggest.status.scored.toLocaleString()} of{" "}
                      {suggest.status.embedded.toLocaleString()} scanned photos
                      · {suggest.status.withheld.toLocaleString()} held back
                    </div>
                  </>
                )}
                <div>
                  Labels: {suggest.status.labels.picks.toLocaleString()} picks ·{" "}
                  {suggest.status.labels.rejects.toLocaleString()} rejects (
                  {suggest.status.labels.unprompted.toLocaleString()} unprompted
                  · {suggest.status.labels.overrides.toLocaleString()} overrides
                  ·{" "}
                  {(
                    suggest.status.labels.agreeLo + suggest.status.labels.agreeHi
                  ).toLocaleString()}{" "}
                  agreements · {suggest.status.labels.batch.toLocaleString()}{" "}
                  bulk)
                </div>
                {suggest.status.modelId != null &&
                  suggest.status.labelsDelta !== 0 && (
                    <div>
                      {suggest.status.labelsDelta > 0 ? "+" : ""}
                      {suggest.status.labelsDelta.toLocaleString()} labels since
                      the last training
                    </div>
                  )}
                {!suggest.status.trainable && (
                  <div style={{ color: "var(--color-t3)" }}>
                    Needs at least 10 picks and 10 rejects on scanned (AI-run)
                    photos before it can learn anything.
                  </div>
                )}
              </div>
            )}
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <button
                onClick={() => void suggest.train()}
                disabled={
                  suggest.status?.running === true ||
                  suggest.status?.trainable === false
                }
                style={{
                  ...btnSecondary,
                  opacity:
                    suggest.status?.running || suggest.status?.trainable === false
                      ? 0.6
                      : 1,
                  cursor: suggest.status?.running ? "default" : "pointer",
                }}
              >
                {suggest.status?.running ? "Training…" : "Train now"}
              </button>
            </div>
            <label
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                fontSize: 12,
                color: "var(--color-t2)",
                marginTop: 10,
                cursor: "pointer",
              }}
            >
              <input
                type="checkbox"
                checked={showSuggestions}
                onChange={(e) => setShowSuggestions(e.target.checked)}
              />
              Show suggestion badges and the "Suggested picks" shelf
            </label>
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

          {/* Catalog backups */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Catalog backups</div>
            <div style={descStyle}>
              A compacted copy of the catalog (index, edits, ratings, keywords —
              not your photo files) is snapshotted automatically once a day and
              kept as the last 7 copies.
            </div>
            <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
              <div style={{ flex: 1, fontSize: 12, color: "var(--color-t2)" }}>
                Last backup:{" "}
                {backup?.lastMs == null ? "never" : fmtAgo(backup.lastMs)} ·{" "}
                {backup?.count ?? 0} kept
              </div>
              <button
                onClick={handleBackupNow}
                disabled={backupBusy}
                style={{
                  ...btnSecondary,
                  opacity: backupBusy ? 0.6 : 1,
                  cursor: backupBusy ? "default" : "pointer",
                }}
              >
                {backupBusy ? "Backing up…" : "Back up now"}
              </button>
            </div>
          </div>

          {/* Diagnostics logs */}
          <div style={sectionStyle}>
            <div style={labelStyle}>Diagnostics logs</div>
            <div style={descStyle}>
              Detailed local logs help debug production issues. Logs are
              redacted to avoid paths, filenames, search text, captions,
              keywords, and people names.
            </div>
            <div
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                marginBottom: 8,
              }}
            >
              <div
                title={logs?.directory}
                style={{
                  flex: 1,
                  minWidth: 0,
                  background: "var(--color-stage)",
                  border: "1px solid var(--color-line-2)",
                  borderRadius: "var(--radius-sm)",
                  color: logs ? "var(--color-t1)" : "var(--color-t3)",
                  padding: "6px 8px",
                  fontSize: 12,
                  fontFamily: "var(--font-mono)",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                }}
              >
                {logs?.directory ?? "Loading…"}
              </div>
              <button
                onClick={() => void handleLogsLocation()}
                disabled={logsBusy}
                style={{ ...btnSecondary, opacity: logsBusy ? 0.6 : 1 }}
              >
                Change…
              </button>
            </div>
            <div style={{ ...descStyle, marginBottom: 8 }}>
              Using {logs ? fmtBytes(logs.sizeBytes) : "…"} across{" "}
              {logs?.fileCount ?? "…"} log file(s).
            </div>
            <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
              {(
                [
                  "error",
                  "warn",
                  "info",
                  "debug",
                  "trace",
                ] as LogsStatus["level"][]
              ).map((level) => (
                <button
                  key={level}
                  onClick={() => handleLogLevel(level)}
                  style={segmentBtn(logs?.level === level)}
                >
                  {level}
                </button>
              ))}
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <button
                onClick={() => void handleExportLogs()}
                disabled={logsBusy}
                style={{ ...btnSecondary, opacity: logsBusy ? 0.6 : 1 }}
              >
                Export ZIP…
              </button>
              <button
                onClick={() => void handleDeleteLogs()}
                disabled={logsBusy}
                style={{
                  ...btnBase,
                  background: confirmLogsDelete
                    ? "#b3261e"
                    : "var(--color-elev)",
                  color: confirmLogsDelete
                    ? "#fff"
                    : "var(--color-danger, #e5685f)",
                  borderColor: confirmLogsDelete
                    ? "#b3261e"
                    : "var(--color-line-2)",
                  opacity: logsBusy ? 0.6 : 1,
                }}
              >
                {confirmLogsDelete
                  ? "Click again to delete logs"
                  : "Delete all logs…"}
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
