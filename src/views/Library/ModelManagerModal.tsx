import { useEffect, useRef, useState } from "react";
import {
  analysisDetectorSize,
  setAnalysisDetectorSize,
  faceStageEnabled,
  setFaceStageEnabled,
  maskAiTierSet,
  type ModelGroup,
  type ModelTierInfo,
  type ModelFileInfo,
  type MaskAiTier,
} from "../../lib/ipc";
import { useAppStore } from "../../store/app";
import { useModelsStore, type DownloadState } from "../../store/models";
import {
  refreshModelOverview,
  startModelDownload,
  cancelModelDownload,
  removeModel,
} from "../../lib/useModelDownloads";
import { ProgressBar } from "../../components/ProgressBar";
import Icon from "../../components/Icon";

const MB = 1024 * 1024;
const GB = 1024 * MB;

function fmtBytes(n: number): string {
  if (n >= GB) return `${(n / GB).toFixed(2)} GB`;
  if (n >= MB) return `${Math.round(n / MB)} MB`;
  return `${Math.max(1, Math.round(n / 1024))} KB`;
}
function fmtSpeed(bps: number): string {
  if (bps >= MB) return `${(bps / MB).toFixed(1)} MB/s`;
  return `${Math.max(1, Math.round(bps / 1024))} KB/s`;
}
function fmtEta(sec: number | null): string {
  if (sec == null || !isFinite(sec)) return "";
  const t = Math.round(sec);
  return `${Math.floor(t / 60)}:${String(t % 60).padStart(2, "0")}`;
}
function dlPct(dl: DownloadState): number {
  if (dl.bytesTotal > 0) return Math.round((dl.bytesDone / dl.bytesTotal) * 100);
  return dl.phase === "done" ? 100 : 0;
}

const btnBase: React.CSSProperties = {
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-sm)",
  padding: "5px 12px",
  fontSize: 12,
  cursor: "pointer",
  whiteSpace: "nowrap",
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
const btnDanger: React.CSSProperties = {
  ...btnBase,
  background: "transparent",
  color: "var(--color-danger)",
  borderColor: "var(--color-danger)",
};
const segmentBtn = (active: boolean): React.CSSProperties => ({
  ...btnBase,
  flex: 1,
  background: active ? "var(--color-accent)" : "var(--color-elev)",
  color: active ? "#fff" : "var(--color-t1)",
  textAlign: "center",
});

function StatusPill({ text, tone }: { text: string; tone: "ok" | "muted" | "err" | "busy" }) {
  const color =
    tone === "ok"
      ? "var(--color-accent)"
      : tone === "err"
        ? "var(--color-danger)"
        : "var(--color-t3)";
  return (
    <span style={{ fontSize: 11, color, fontWeight: 500 }}>{text}</span>
  );
}

/** One expandable file row inside a card / tier's detail. */
function FileRow({ f }: { f: ModelFileInfo }) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        fontSize: 11,
        color: "var(--color-t3)",
        padding: "2px 0",
      }}
    >
      <span style={{ width: 12, color: f.present ? "var(--color-accent)" : "var(--color-t3)" }}>
        {f.present ? "✓" : "·"}
      </span>
      <span
        style={{
          flex: 1,
          fontFamily: "var(--font-mono)",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
        }}
      >
        {f.rel}
      </span>
      <span>{fmtBytes(f.sizeBytes)}</span>
    </div>
  );
}

/**
 * AI Models manager: one capability card per group (Detection & Scene / People / AI Masking) with
 * install state, live byte-level download progress + speed/ETA, download/cancel/inline-confirm-remove,
 * expandable per-file (and per-SAM-tier) detail, and the relocated per-capability AI settings. Driven
 * by the app store's `modelManagerOpen`; reads live state from the shared models store.
 */
export default function ModelManagerModal() {
  const open = useAppStore((s) => s.modelManagerOpen);
  const setOpen = useAppStore((s) => s.setModelManagerOpen);
  const overview = useModelsStore((s) => s.overview);
  const downloads = useModelsStore((s) => s.downloads);

  const [detectorSize, setDetSize] = useState<number | null>(null);
  const [faceStage, setFaceStage] = useState<boolean | null>(null);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [confirm, setConfirm] = useState<string | null>(null);
  const confirmTimer = useRef<number | null>(null);

  useEffect(() => {
    if (!open) return;
    void refreshModelOverview();
    void analysisDetectorSize().then(setDetSize).catch(() => {});
    void faceStageEnabled().then(setFaceStage).catch(() => {});
    setConfirm(null);
  }, [open]);

  useEffect(
    () => () => {
      if (confirmTimer.current !== null) window.clearTimeout(confirmTimer.current);
    },
    [],
  );

  if (!open) return null;

  const accelerator = overview.find((g) => g.available)?.accelerator ?? null;

  const armConfirm = (key: string) => {
    setConfirm(key);
    if (confirmTimer.current !== null) window.clearTimeout(confirmTimer.current);
    confirmTimer.current = window.setTimeout(() => setConfirm(null), 3000);
  };
  const onRemove = (key: string, id: ModelGroup["id"], tier?: MaskAiTier) => {
    if (confirm === key) {
      setConfirm(null);
      void removeModel(id, tier);
    } else {
      armConfirm(key);
    }
  };
  const removeBtn = (keyId: string, id: ModelGroup["id"], tier?: MaskAiTier) => (
    <button style={btnDanger} onClick={() => onRemove(keyId, id, tier)}>
      {confirm === keyId ? "Confirm remove?" : "Remove"}
    </button>
  );

  const onTier = (tier: MaskAiTier) => {
    void maskAiTierSet(tier).then(refreshModelOverview);
  };

  const toggle = (id: string) => setExpanded((e) => ({ ...e, [id]: !e[id] }));

  return (
    <div
      onClick={(e) => {
        if (e.target === e.currentTarget) setOpen(false);
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,.5)",
        backdropFilter: "blur(3px)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        zIndex: 55,
      }}
    >
      <div
        style={{
          width: 560,
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
            gap: 10,
            flexShrink: 0,
          }}
        >
          <span style={{ fontSize: 14, fontWeight: 600, color: "var(--color-t1)" }}>
            AI Models
          </span>
          <span style={{ flex: 1 }} />
          {accelerator && (
            <span
              style={{
                fontSize: 10.5,
                fontFamily: "var(--font-mono)",
                color: "var(--color-t2)",
                border: "1px solid var(--color-line-2)",
                borderRadius: "var(--radius-sm)",
                padding: "2px 7px",
              }}
              title="Hardware accelerator used for on-device AI"
            >
              {accelerator}
            </span>
          )}
          <button
            onClick={() => setOpen(false)}
            title="Close"
            style={{
              background: "none",
              border: "none",
              color: "var(--color-t3)",
              fontSize: 18,
              lineHeight: 1,
              cursor: "pointer",
              padding: "2px 4px",
            }}
          >
            ✕
          </button>
        </div>

        {/* Body */}
        <div style={{ overflowY: "auto", flex: 1 }}>
          {overview.length === 0 && (
            <div style={{ padding: 24, fontSize: 12, color: "var(--color-t3)" }}>
              Loading…
            </div>
          )}
          {overview.map((g) => {
            const dl = downloads[g.id];
            const downloading = !!dl && !dl.error;
            const isOpen = !!expanded[g.id];
            return (
              <div
                key={g.id}
                style={{ padding: "16px 20px", borderBottom: "1px solid var(--color-line)" }}
              >
                {/* Title row */}
                <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                  <button
                    onClick={() => toggle(g.id)}
                    style={{
                      background: "none",
                      border: "none",
                      color: "var(--color-t3)",
                      cursor: "pointer",
                      padding: 0,
                      transform: isOpen ? "rotate(90deg)" : "none",
                      transition: "transform .15s",
                      display: "flex",
                    }}
                    title={isOpen ? "Hide files" : "Show files"}
                  >
                    <Icon name="chev" size={12} />
                  </button>
                  <div style={{ flex: 1, minWidth: 0 }}>
                    <div style={{ fontSize: 13, fontWeight: 500, color: "var(--color-t1)" }}>
                      {g.name}
                    </div>
                    <div style={{ fontSize: 11, color: "var(--color-t3)", marginTop: 1 }}>
                      {g.description}
                    </div>
                  </div>
                  <div style={{ textAlign: "right", flexShrink: 0 }}>
                    <div style={{ fontSize: 11, color: "var(--color-t2)" }}>
                      {fmtBytes(g.installed ? g.sizeBytes : g.approxTotalBytes)}
                    </div>
                    <div>
                      {!g.available ? (
                        <StatusPill text="Unavailable" tone="muted" />
                      ) : dl?.error ? (
                        <StatusPill text="Error" tone="err" />
                      ) : downloading ? (
                        <StatusPill text={`${dlPct(dl!)}%`} tone="busy" />
                      ) : g.installed ? (
                        <StatusPill text="Installed ✓" tone="ok" />
                      ) : (
                        <StatusPill text="Not installed" tone="muted" />
                      )}
                    </div>
                  </div>
                </div>

                {/* SAM tier selector */}
                {g.available && g.id === "mask_ai" && g.activeTier && (
                  <div style={{ display: "flex", gap: 6, marginTop: 12 }}>
                    {g.tiers.map((t) => (
                      <button
                        key={t.tier}
                        onClick={() => onTier(t.tier)}
                        style={segmentBtn(g.activeTier === t.tier)}
                        title={`${t.label} · ${fmtBytes(t.sizeBytes)}`}
                      >
                        {t.tier === "realtime"
                          ? "Realtime"
                          : t.tier === "balanced"
                            ? "Balanced"
                            : "Max"}
                      </button>
                    ))}
                  </div>
                )}

                {/* Detection resolution */}
                {g.available && g.id === "analysis" && detectorSize != null && (
                  <SettingRow label="Animal detection resolution">
                    {([1280, 640] as number[]).map((s) => (
                      <button
                        key={s}
                        onClick={() =>
                          void setAnalysisDetectorSize(s).then(() => setDetSize(s))
                        }
                        style={segmentBtn(detectorSize === s)}
                      >
                        {s === 1280 ? "1280 · best" : "640 · fast"}
                      </button>
                    ))}
                  </SettingRow>
                )}

                {/* Face stage toggle */}
                {g.available && g.id === "faces" && faceStage != null && (
                  <SettingRow label="Detect people during scan">
                    {([true, false] as boolean[]).map((on) => (
                      <button
                        key={String(on)}
                        onClick={() =>
                          void setFaceStageEnabled(on).then(() => setFaceStage(on))
                        }
                        style={segmentBtn(faceStage === on)}
                      >
                        {on ? "On" : "Off"}
                      </button>
                    ))}
                  </SettingRow>
                )}

                {/* Live progress */}
                {g.available && downloading && dl && (
                  <div style={{ marginTop: 12 }}>
                    <ProgressBar pct={dlPct(dl)} height={4} />
                    <div
                      style={{
                        display: "flex",
                        justifyContent: "space-between",
                        fontSize: 10.5,
                        color: "var(--color-t3)",
                        marginTop: 4,
                      }}
                    >
                      <span>
                        {fmtBytes(dl.bytesDone)} / {fmtBytes(dl.bytesTotal)}
                        {dl.fileCount > 0 && ` · file ${dl.fileIndex}/${dl.fileCount}`}
                      </span>
                      <span>
                        {dl.bytesPerSec > 0 && fmtSpeed(dl.bytesPerSec)}
                        {dl.etaSec != null && ` · ${fmtEta(dl.etaSec)}`}
                      </span>
                    </div>
                  </div>
                )}

                {/* Error */}
                {g.available && dl?.error && (
                  <div
                    style={{
                      marginTop: 10,
                      fontSize: 11,
                      color: "var(--color-danger)",
                      lineHeight: 1.5,
                    }}
                  >
                    {dl.error}
                  </div>
                )}

                {/* License note */}
                {g.available && g.license && (
                  <div style={{ marginTop: 10, fontSize: 10.5, color: "var(--color-t3)" }}>
                    {g.license}
                  </div>
                )}

                {/* Actions */}
                {g.available && (
                  <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
                    {downloading ? (
                      <button
                        style={btnSecondary}
                        onClick={() => cancelModelDownload(g.id)}
                      >
                        Stop
                      </button>
                    ) : g.installed ? (
                      removeBtn(`${g.id}:active`, g.id)
                    ) : (
                      <button
                        style={btnAccent}
                        onClick={() => void startModelDownload(g.id)}
                      >
                        Download {fmtBytes(g.approxTotalBytes)}
                      </button>
                    )}
                  </div>
                )}

                {/* Expanded detail */}
                {isOpen && g.available && (
                  <div
                    style={{
                      marginTop: 12,
                      paddingTop: 10,
                      borderTop: "1px solid var(--color-line)",
                    }}
                  >
                    {g.id === "mask_ai" && g.tiers.length > 0
                      ? g.tiers.map((t: ModelTierInfo) => (
                          <div key={t.tier} style={{ marginBottom: 10 }}>
                            <div
                              style={{
                                display: "flex",
                                alignItems: "center",
                                gap: 8,
                                marginBottom: 3,
                              }}
                            >
                              <span
                                style={{ fontSize: 11.5, color: "var(--color-t2)", flex: 1 }}
                              >
                                {t.label} · {fmtBytes(t.sizeBytes)}
                              </span>
                              {t.installed ? (
                                removeBtn(`mask_ai:${t.tier}`, "mask_ai", t.tier)
                              ) : (
                                <StatusPill text="Not installed" tone="muted" />
                              )}
                            </div>
                            {t.files.map((f) => (
                              <FileRow key={f.rel} f={f} />
                            ))}
                          </div>
                        ))
                      : g.files.map((f) => <FileRow key={f.rel} f={f} />)}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

/** A labelled row of inline segment/toggle controls relocated from Settings. */
function SettingRow({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div style={{ marginTop: 12 }}>
      <div style={{ fontSize: 10.5, color: "var(--color-t3)", marginBottom: 5 }}>
        {label}
      </div>
      <div style={{ display: "flex", gap: 6 }}>{children}</div>
    </div>
  );
}
