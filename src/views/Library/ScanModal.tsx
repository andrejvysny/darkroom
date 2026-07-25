import { useEffect, useState } from "react";
import Icon from "../../components/Icon";
import {
  analysisModelsEnsure,
  facesModelsEnsure,
  scanPrefsGet,
  scanPrefsSet,
  SCAN_STAGES,
  type ScanScope,
  type ScopeCounts,
  type StageId,
} from "../../lib/ipc";
import { log } from "../../lib/logger";
import { useScan } from "../../lib/useScan";
import type { ScanScopeState } from "../../lib/useScanScope";

interface ScanModalProps {
  open: boolean;
  onClose: () => void;
  /** Scope derived from the live library filter, plus per-stage pending counts. */
  scanScope: ScanScopeState;
  /** Re-price after models are installed or a scan finishes. */
  onRefreshCounts: () => void;
}

/**
 * The single place scans are configured: which stages, over what, and whether to redo finished work.
 *
 * Replaces the three separate button clusters that used to live in the PEOPLE, PANORAMAS and
 * DETECTED sidebar headers — those implied the passes were independent, when faces and object
 * detection are in fact one backend job.
 */
export default function ScanModal({
  open,
  onClose,
  scanScope,
  onRefreshCounts,
}: ScanModalProps) {
  const scan = useScan();
  const [picked, setPicked] = useState<StageId[]>([]);
  const [force, setForce] = useState(false);
  const [wholeLibrary, setWholeLibrary] = useState(false);
  const [installing, setInstalling] = useState<StageId | null>(null);

  // Reset transient state and load the remembered ticks each time the modal opens.
  useEffect(() => {
    if (!open) return;
    setForce(false);
    setWholeLibrary(false);
    setInstalling(null);
    void scanPrefsGet()
      .then(setPicked)
      .catch((err) => log.debug("scan", "prefs load failed", log.errorSummary(err)));
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const counts: ScopeCounts | null = scanScope.counts;
  const scoped = scanScope.label != null && !wholeLibrary;
  const effectiveScope: ScanScope | null = scoped ? scanScope.scope : null;
  const readyOf = (id: StageId) =>
    counts?.stages.find((s) => s.stage === id)?.modelsReady ?? true;
  const pendingOf = (id: StageId) =>
    counts?.stages.find((s) => s.stage === id)?.pending ?? null;

  const selectable = picked.filter(readyOf);
  const canRun = selectable.length > 0 && !scan.running;

  function toggle(id: StageId) {
    setPicked((prev) =>
      prev.includes(id) ? prev.filter((s) => s !== id) : [...prev, id],
    );
  }

  async function install(id: StageId) {
    setInstalling(id);
    try {
      await (id === "faces" ? facesModelsEnsure() : analysisModelsEnsure());
      onRefreshCounts();
    } catch (err) {
      log.warn("scan", "model install failed", { id, ...log.errorSummary(err) });
    } finally {
      setInstalling(null);
    }
  }

  async function start() {
    // Persist the ticks (including unavailable ones, so they come back once models are installed).
    void scanPrefsSet(picked).catch(() => {});
    onClose();
    await scan.run(
      { stages: selectable, scope: effectiveScope, force },
      scoped ? scanScope.label : null,
    );
    onRefreshCounts();
  }

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
        zIndex: 60,
      }}
    >
      <div
        data-testid="scan-modal"
        style={{
          width: 460,
          maxWidth: "94vw",
          maxHeight: "88vh",
          overflowY: "auto",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.7)",
        }}
      >
        <div
          style={{
            padding: "12px 20px",
            borderBottom: "1px solid var(--color-line)",
            display: "flex",
            alignItems: "center",
            gap: 8,
            fontSize: 14,
            fontWeight: 600,
            color: "var(--color-t1)",
          }}
        >
          <Icon name="scan" size={14} />
          Run AI scan
        </div>

        <div
          style={{
            padding: "14px 20px",
            display: "flex",
            flexDirection: "column",
            gap: 2,
          }}
        >
          {SCAN_STAGES.map((s) => {
            const ready = readyOf(s.id);
            const pending = pendingOf(s.id);
            const checked = picked.includes(s.id) && ready;
            return (
              <div
                key={s.id}
                style={{
                  display: "flex",
                  alignItems: "flex-start",
                  gap: 9,
                  padding: "7px 0",
                  opacity: ready ? 1 : 0.6,
                }}
              >
                <input
                  type="checkbox"
                  id={`stage-${s.id}`}
                  data-testid={`stage-${s.id}`}
                  checked={checked}
                  disabled={!ready}
                  onChange={() => toggle(s.id)}
                  style={{ accentColor: "var(--color-accent)", marginTop: 2 }}
                />
                <label
                  htmlFor={`stage-${s.id}`}
                  style={{
                    flex: 1,
                    fontSize: 12.5,
                    color: "var(--color-t1)",
                    cursor: ready ? "pointer" : "default",
                    userSelect: "none",
                  }}
                >
                  {s.label}
                  {s.hint && (
                    <div style={{ fontSize: 11, color: "var(--color-t3)", marginTop: 2 }}>
                      {s.hint}
                    </div>
                  )}
                </label>
                {ready ? (
                  <span
                    style={{ fontSize: 11.5, color: "var(--color-t3)", whiteSpace: "nowrap" }}
                  >
                    {pending == null
                      ? ""
                      : pending === 0
                        ? "up to date"
                        : `${pending.toLocaleString()} pending`}
                  </span>
                ) : (
                  <button
                    onClick={() => void install(s.id)}
                    disabled={installing != null}
                    data-testid={`install-${s.id}`}
                    style={{
                      border: "1px solid var(--color-line-2)",
                      background: "var(--color-elev)",
                      color: "var(--color-t1)",
                      borderRadius: "var(--radius-sm)",
                      padding: "2px 8px",
                      fontSize: 11,
                      whiteSpace: "nowrap",
                      cursor: installing != null ? "default" : "pointer",
                    }}
                  >
                    {installing === s.id ? "Installing…" : "Install…"}
                  </button>
                )}
              </div>
            );
          })}
        </div>

        <div
          style={{
            padding: "12px 20px",
            borderTop: "1px solid var(--color-line)",
            display: "flex",
            flexDirection: "column",
            gap: 10,
          }}
        >
          {scanScope.label && (
            <div style={{ display: "flex", alignItems: "center", gap: 8, fontSize: 12 }}>
              <span style={{ color: "var(--color-t3)" }}>Scan</span>
              <select
                data-testid="scan-scope"
                value={wholeLibrary ? "all" : "scope"}
                onChange={(e) => setWholeLibrary(e.target.value === "all")}
                style={{
                  flex: 1,
                  background: "var(--color-elev)",
                  border: "1px solid var(--color-line-2)",
                  borderRadius: "var(--radius-sm)",
                  color: "var(--color-t1)",
                  fontSize: 12,
                  padding: "5px 7px",
                }}
              >
                <option value="scope">
                  {scanScope.label}
                  {counts ? ` (${counts.total.toLocaleString()})` : ""}
                </option>
                <option value="all">Whole library</option>
              </select>
            </div>
          )}
          <label
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              fontSize: 12,
              color: "var(--color-t2)",
              cursor: "pointer",
              userSelect: "none",
            }}
          >
            <input
              type="checkbox"
              data-testid="scan-force"
              checked={force}
              onChange={(e) => setForce(e.target.checked)}
              style={{ accentColor: "var(--color-accent)" }}
            />
            Re-run photos already scanned
          </label>
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
            onClick={onClose}
            style={{
              border: "1px solid var(--color-line-2)",
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 14px",
              fontSize: 12,
              cursor: "pointer",
            }}
          >
            Cancel
          </button>
          <button
            onClick={() => void start()}
            disabled={!canRun}
            data-testid="scan-start"
            style={{
              border: "none",
              background: canRun ? "var(--color-accent)" : "var(--color-line-2)",
              color: "#fff",
              borderRadius: "var(--radius-sm)",
              padding: "6px 16px",
              fontSize: 12,
              cursor: canRun ? "pointer" : "default",
              opacity: canRun ? 1 : 0.6,
            }}
          >
            {scan.running ? "Scanning…" : "Run"}
          </button>
        </div>
      </div>
    </div>
  );
}
