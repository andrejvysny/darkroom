import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { useAppStore } from "../../store/app";
import {
  previewName,
  runExportBatch,
  type ExportFormat,
} from "../../lib/export";

interface Preset {
  name: string;
  template: string;
}

// Non-deletable starter templates always shown first.
const BUILTIN_PRESETS: Preset[] = [
  { name: "Original name", template: "{original}" },
  { name: "Date + name", template: "{date}_{original}" },
  { name: "Name + sequence", template: "{original}_{seq}" },
];

const LS_DIR = "darkroom.export.dir";
const LS_PRESETS = "darkroom.export.presets";
const LS_LAST = "darkroom.export.last"; // { template, format, quality }

function loadPresets(): Preset[] {
  try {
    const raw = localStorage.getItem(LS_PRESETS);
    if (!raw) return [];
    const arr = JSON.parse(raw) as Preset[];
    return Array.isArray(arr)
      ? arr.filter((p) => p && typeof p.name === "string" && typeof p.template === "string")
      : [];
  } catch {
    return [];
  }
}

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
  width: 460,
  maxWidth: "92vw",
  maxHeight: "88vh",
  overflowY: "auto",
  background: "#26262a",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-lg)",
  boxShadow: "0 24px 80px rgba(0,0,0,.6)",
};

const input: React.CSSProperties = {
  width: "100%",
  background: "var(--color-elev)",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-sm)",
  padding: "7px 9px",
  fontSize: 13,
  color: "var(--color-t1)",
  outline: "none",
};

const fieldLabel: React.CSSProperties = {
  fontSize: 11,
  color: "var(--color-t3)",
  marginBottom: 5,
};

/**
 * Export modal (single image or batch). Pick destination folder, filename template (with saved
 * named presets), format and JPEG quality. Driven by the store's `exportTargets` (null = closed).
 */
export default function ExportModal() {
  const targets = useAppStore((s) => s.exportTargets);
  const setTargets = useAppStore((s) => s.setExportTargets);
  const open_ = targets !== null;

  const [dir, setDir] = useState<string | null>(null);
  const [template, setTemplate] = useState("{original}");
  const [format, setFormat] = useState<ExportFormat>("jpeg");
  const [quality, setQuality] = useState(92);
  const [saved, setSaved] = useState<Preset[]>([]);
  const [presetSel, setPresetSel] = useState("");
  const [newName, setNewName] = useState("");
  const [busy, setBusy] = useState(false);

  // Seed from localStorage each time the modal opens.
  useEffect(() => {
    if (!open_) return;
    setSaved(loadPresets());
    setDir(localStorage.getItem(LS_DIR));
    setBusy(false);
    setNewName("");
    setPresetSel("");
    try {
      const last = JSON.parse(localStorage.getItem(LS_LAST) ?? "null") as {
        template?: string;
        format?: ExportFormat;
        quality?: number;
      } | null;
      setTemplate(last?.template ?? "{original}");
      setFormat(last?.format === "png" ? "png" : "jpeg");
      setQuality(
        typeof last?.quality === "number"
          ? Math.min(100, Math.max(1, last.quality))
          : 92,
      );
    } catch {
      setTemplate("{original}");
      setFormat("jpeg");
      setQuality(92);
    }
  }, [open_]);

  const presets = useMemo(() => [...BUILTIN_PRESETS, ...saved], [saved]);
  const isSavedName = (name: string) => saved.some((p) => p.name === name);

  const preview = useMemo(() => {
    if (!targets || targets.length === 0) return "";
    const ext = format === "png" ? "png" : "jpg";
    const first = `${previewName(targets[0], template, 1)}.${ext}`;
    return targets.length > 1
      ? `${first}  (+${targets.length - 1} more)`
      : first;
  }, [targets, template, format]);

  if (!open_ || !targets) return null;

  const close = () => setTargets(null);

  const pickFolder = async () => {
    const picked = await open({ directory: true, title: "Export destination" });
    if (typeof picked === "string") setDir(picked);
  };

  const applyPreset = (name: string) => {
    setPresetSel(name);
    const p = presets.find((x) => x.name === name);
    if (p) setTemplate(p.template);
  };

  const savePreset = () => {
    const name = newName.trim();
    if (!name) return;
    // Overwrite an existing saved preset of the same name; never shadow a built-in.
    const next = [
      ...saved.filter((p) => p.name !== name),
      { name, template },
    ];
    setSaved(next);
    localStorage.setItem(LS_PRESETS, JSON.stringify(next));
    setPresetSel(name);
    setNewName("");
  };

  const deletePreset = () => {
    if (!isSavedName(presetSel)) return;
    const next = saved.filter((p) => p.name !== presetSel);
    setSaved(next);
    localStorage.setItem(LS_PRESETS, JSON.stringify(next));
    setPresetSel("");
  };

  const canExport = !!dir && template.trim().length > 0 && !busy;

  const doExport = async () => {
    if (!dir || !canExport) return;
    setBusy(true);
    localStorage.setItem(LS_DIR, dir);
    localStorage.setItem(
      LS_LAST,
      JSON.stringify({ template, format, quality }),
    );
    await runExportBatch(targets, { dir, template, format, quality });
    close();
  };

  return (
    <div
      style={overlay}
      onClick={(e) => {
        if (e.target === e.currentTarget && !busy) close();
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
          Export {targets.length > 1 ? `${targets.length} photos` : "photo"}
        </div>

        <div
          style={{
            padding: "16px 18px",
            display: "flex",
            flexDirection: "column",
            gap: 14,
          }}
        >
          {/* Destination */}
          <div>
            <div style={fieldLabel}>Destination folder</div>
            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <div
                style={{
                  ...input,
                  flex: 1,
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  color: dir ? "var(--color-t1)" : "var(--color-t3)",
                }}
                title={dir ?? undefined}
              >
                {dir ?? "Choose a folder…"}
              </div>
              <button
                onClick={pickFolder}
                style={{
                  border: "1px solid var(--color-line-2)",
                  background: "var(--color-elev)",
                  color: "var(--color-t1)",
                  borderRadius: "var(--radius-sm)",
                  padding: "7px 12px",
                  fontSize: 12,
                  cursor: "pointer",
                  flexShrink: 0,
                }}
              >
                Browse…
              </button>
            </div>
          </div>

          {/* Preset picker */}
          <div>
            <div style={fieldLabel}>Naming preset</div>
            <div style={{ display: "flex", gap: 8 }}>
              <select
                value={presetSel}
                onChange={(e) => applyPreset(e.target.value)}
                style={{ ...input, flex: 1 }}
              >
                <option value="">Custom…</option>
                {presets.map((p) => (
                  <option key={p.name} value={p.name}>
                    {p.name}
                    {isSavedName(p.name) ? "" : "  (built-in)"}
                  </option>
                ))}
              </select>
              <button
                onClick={deletePreset}
                disabled={!isSavedName(presetSel)}
                style={{
                  border: "1px solid var(--color-line-2)",
                  background: "var(--color-elev)",
                  color: isSavedName(presetSel)
                    ? "var(--color-t1)"
                    : "var(--color-t3)",
                  borderRadius: "var(--radius-sm)",
                  padding: "7px 12px",
                  fontSize: 12,
                  cursor: isSavedName(presetSel) ? "pointer" : "default",
                  flexShrink: 0,
                }}
              >
                Delete
              </button>
            </div>
          </div>

          {/* Template */}
          <div>
            <div style={fieldLabel}>
              Filename template — tokens: {"{original}"} {"{date}"} {"{seq}"}
            </div>
            <input
              value={template}
              onChange={(e) => {
                setTemplate(e.target.value);
                setPresetSel("");
              }}
              placeholder="{original}"
              style={input}
            />
            <div
              style={{
                fontSize: 11,
                color: "var(--color-t2)",
                marginTop: 6,
                fontFamily: "var(--font-mono)",
                whiteSpace: "nowrap",
                overflow: "hidden",
                textOverflow: "ellipsis",
              }}
              title={preview}
            >
              → {preview}
            </div>
            {/* Save current template as a named preset */}
            <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
              <input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && savePreset()}
                placeholder="Save current as preset…"
                style={{ ...input, flex: 1 }}
              />
              <button
                onClick={savePreset}
                disabled={newName.trim().length === 0}
                style={{
                  border: "1px solid var(--color-line-2)",
                  background: "var(--color-elev)",
                  color:
                    newName.trim().length > 0
                      ? "var(--color-t1)"
                      : "var(--color-t3)",
                  borderRadius: "var(--radius-sm)",
                  padding: "7px 12px",
                  fontSize: 12,
                  cursor: newName.trim().length > 0 ? "pointer" : "default",
                  flexShrink: 0,
                }}
              >
                Save
              </button>
            </div>
          </div>

          {/* Format */}
          <div>
            <div style={fieldLabel}>Format</div>
            <div style={{ display: "flex", gap: 16 }}>
              {(["jpeg", "png"] as const).map((f) => (
                <label
                  key={f}
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 7,
                    fontSize: 12.5,
                    color: "var(--color-t1)",
                    cursor: "pointer",
                  }}
                >
                  <input
                    type="radio"
                    name="export-format"
                    checked={format === f}
                    onChange={() => setFormat(f)}
                  />
                  {f === "jpeg" ? "JPEG" : "PNG"}
                </label>
              ))}
            </div>
          </div>

          {/* Quality (JPEG only) */}
          <div style={{ opacity: format === "jpeg" ? 1 : 0.4 }}>
            <div style={fieldLabel}>JPEG quality — {quality}</div>
            <input
              type="range"
              min={1}
              max={100}
              value={quality}
              disabled={format !== "jpeg"}
              onChange={(e) => setQuality(Number(e.target.value))}
              style={{ width: "100%" }}
            />
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
            onClick={doExport}
            disabled={!canExport}
            style={{
              border: "none",
              background: canExport
                ? "var(--color-accent)"
                : "var(--color-line-2)",
              color: "#fff",
              borderRadius: "var(--radius-sm)",
              padding: "6px 16px",
              fontSize: 12,
              cursor: canExport ? "pointer" : "default",
              opacity: canExport ? 1 : 0.6,
            }}
          >
            {busy ? "Exporting…" : "Export"}
          </button>
        </div>
      </div>
    </div>
  );
}
