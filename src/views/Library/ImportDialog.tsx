import { useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import Icon from "../../components/Icon";
import {
  importDedup,
  importList,
  importThumb,
  type CollectionRow,
  type DedupResult,
  type ImportMode,
  type ImportOptions,
  type SourceFile,
} from "../../lib/ipc";
import { commitImport, pickFolder, resolveDest } from "../../lib/importFlow";

interface Props {
  open: boolean;
  collections: CollectionRow[];
  onClose: () => void;
  onComplete: () => void;
}

const MODES: { mode: ImportMode; label: string; desc: string }[] = [
  {
    mode: "copy",
    label: "Copy",
    desc: "Copy into the library (routed by date); source untouched.",
  },
  {
    mode: "move",
    label: "Move",
    desc: "Copy + verify, then Trash the originals.",
  },
  {
    mode: "reference",
    label: "Reference",
    desc: "Catalog in place; source folder is watched.",
  },
];

function isDup(s: SourceFile["status"]): boolean {
  return s === "duplicateLibrary" || s === "duplicateBatch";
}

const DUP_BADGE: Record<SourceFile["status"], string | null> = {
  pending: null,
  new: null,
  duplicateLibrary: "in library",
  duplicateBatch: "duplicate",
};

function selectable(f: SourceFile, skipDuplicates: boolean): boolean {
  return !(skipDuplicates && isDup(f.status));
}

/** UTC day from epoch seconds; "Unknown" for missing mtime. */
function dayOf(f: SourceFile): string {
  if (!f.mtime) return "Unknown";
  return new Date(f.mtime * 1000).toISOString().slice(0, 10);
}

function fmtSize(bytes: number): string {
  return bytes >= 1048576
    ? `${(bytes / 1048576).toFixed(1)} MB`
    : `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

/** Group files by mtime day, newest first, "Unknown" last. */
function groupByDay(
  files: SourceFile[],
): { day: string; items: SourceFile[] }[] {
  const map = new Map<string, SourceFile[]>();
  for (const f of files) {
    const d = dayOf(f);
    (map.get(d) ?? map.set(d, []).get(d)!).push(f);
  }
  return [...map.entries()]
    .sort(([a], [b]) =>
      a === "Unknown" ? 1 : b === "Unknown" ? -1 : a < b ? 1 : a > b ? -1 : 0,
    )
    .map(([day, items]) => ({ day, items }));
}

export default function ImportDialog({
  open,
  collections,
  onClose,
  onComplete,
}: Props) {
  const [source, setSource] = useState<string | null>(null);
  const [dest, setDest] = useState<string | null>(null);
  const [files, setFiles] = useState<SourceFile[]>([]);
  const [listing, setListing] = useState(false);
  const [recursive, setRecursive] = useState(true);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [dedupProgress, setDedupProgress] = useState<{
    done: number;
    total: number;
  } | null>(null);
  const dedupReqRef = useRef(0);
  const dedupUnlistenRef = useRef<(() => void) | null>(null);

  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [previewError, setPreviewError] = useState(false);
  const previewUrlRef = useRef<string | null>(null);
  const previewReqRef = useRef(0);

  const [skipDuplicates, setSkipDuplicates] = useState(true);
  const skipDupRef = useRef(true);
  skipDupRef.current = skipDuplicates;
  const [mode, setMode] = useState<ImportMode>("copy");
  const [rating, setRating] = useState(0);
  const [flag, setFlag] = useState<"none" | "pick" | "reject">("none");
  const [keywordsText, setKeywordsText] = useState("");
  const [collChoice, setCollChoice] = useState<string>("");
  const [newCollName, setNewCollName] = useState("");

  function revokePreview() {
    if (previewUrlRef.current) URL.revokeObjectURL(previewUrlRef.current);
    previewUrlRef.current = null;
  }

  // Reset everything each time the dialog opens; resolve the default destination up front.
  useEffect(() => {
    if (!open) return;
    setSource(null);
    setFiles([]);
    setSelected(new Set());
    setListing(false);
    setRecursive(true);
    setDedupProgress(null);
    setPreviewPath(null);
    setPreviewUrl(null);
    setPreviewError(false);
    revokePreview();
    void resolveDest().then(setDest);
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
      revokePreview();
      dedupReqRef.current++; // cancel any in-flight dedup
      dedupUnlistenRef.current?.();
      dedupUnlistenRef.current = null;
    };
  }, [open, onClose]);

  /** Merge resolved dedup verdicts into the file list; auto-deselect duplicates when skip is on. */
  function applyDedup(results: DedupResult[]) {
    if (results.length === 0) return;
    const byPath = new Map(results.map((r) => [r.path, r.status]));
    setFiles((prev) =>
      prev.map((f) =>
        byPath.has(f.path) ? { ...f, status: byPath.get(f.path)! } : f,
      ),
    );
    if (skipDupRef.current)
      setSelected((prev) => {
        const next = new Set(prev);
        for (const r of results) if (isDup(r.status)) next.delete(r.path);
        return next;
      });
  }

  /** Background hash-verified dedup: streams verdicts in (auto-deselecting dups) while the list stays
   *  fully interactive. A new list/source bumps the request token so stale results are ignored. */
  async function runDedup(list: SourceFile[]) {
    const req = ++dedupReqRef.current;
    dedupUnlistenRef.current?.();
    setDedupProgress({ done: 0, total: list.length });
    const un = await listen<{
      done: number;
      total: number;
      results: DedupResult[];
    }>("import:dedup:progress", (ev) => {
      if (req !== dedupReqRef.current) return;
      applyDedup(ev.payload.results);
      setDedupProgress({ done: ev.payload.done, total: ev.payload.total });
    });
    dedupUnlistenRef.current = un;
    try {
      const all = await importDedup(list.map((f) => f.path));
      if (req === dedupReqRef.current) applyDedup(all);
    } finally {
      un();
      if (req === dedupReqRef.current) {
        setDedupProgress(null);
        if (dedupUnlistenRef.current === un) dedupUnlistenRef.current = null;
      }
    }
  }

  /** List a source (fast, metadata-only), select all, then kick off background hash dedup. */
  async function loadList(src: string, rec: boolean) {
    setListing(true);
    setPreviewPath(null);
    setPreviewUrl(null);
    revokePreview();
    try {
      const list = await importList(src, rec);
      setFiles(list);
      setSelected(new Set(list.map((f) => f.path))); // all selected; dedup deselects duplicates
      void runDedup(list);
    } finally {
      setListing(false);
    }
  }

  async function chooseSource() {
    const picked = await pickFolder("Select import source (card / folder)");
    if (!picked) return;
    setSource(picked);
    await loadList(picked, recursive);
  }

  function toggleRecursive(v: boolean) {
    setRecursive(v);
    if (source) void loadList(source, v);
  }

  // Lazily load the clicked file's embedded preview.
  async function preview(path: string) {
    setPreviewPath(path);
    setPreviewError(false);
    const req = ++previewReqRef.current;
    try {
      const url = await importThumb(path);
      if (req !== previewReqRef.current) {
        URL.revokeObjectURL(url); // a newer click superseded this one
        return;
      }
      revokePreview();
      previewUrlRef.current = url;
      setPreviewUrl(url);
    } catch {
      if (req === previewReqRef.current) setPreviewError(true);
    }
  }

  const groups = useMemo(() => groupByDay(files), [files]);
  const dupCount = useMemo(
    () => files.filter((f) => isDup(f.status)).length,
    [files],
  );
  const selectableCount = files.filter((f) =>
    selectable(f, skipDuplicates),
  ).length;

  function toggle(path: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  function toggleSkipDuplicates(v: boolean) {
    setSkipDuplicates(v);
    if (v)
      setSelected((prev) => {
        const next = new Set(prev);
        for (const f of files) if (isDup(f.status)) next.delete(f.path);
        return next;
      });
  }

  const setAll = (pred: (f: SourceFile) => boolean) =>
    setSelected(new Set(files.filter(pred).map((f) => f.path)));

  function doImport() {
    if (!source || selected.size === 0) return;
    const keywords = keywordsText
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const options: ImportOptions = {
      rating: rating > 0 ? rating : null,
      flag: flag !== "none" ? flag : null,
      keywords,
      collectionId:
        collChoice && collChoice !== "new" ? Number(collChoice) : null,
      newCollection:
        collChoice === "new" && newCollName.trim() ? newCollName.trim() : null,
    };
    onClose();
    void commitImport(
      source,
      mode,
      dest ?? "",
      [...selected],
      options,
      onComplete,
    );
  }

  if (!open) return null;

  return (
    <div style={overlay()}>
      <div style={panel()}>
        {/* Header */}
        <div style={header()}>
          <span
            style={{
              fontWeight: 600,
              fontSize: 13.5,
              color: "var(--color-t1)",
            }}
          >
            Import
          </span>
          {source && (
            <>
              <span title={source} style={pathLabel()}>
                {source}
              </span>
              <button onClick={chooseSource} style={linkBtn()}>
                Change…
              </button>
            </>
          )}
          {files.length > 0 && (
            <span
              style={{
                marginLeft: "auto",
                fontSize: 11.5,
                color: "var(--color-t3)",
              }}
            >
              {dedupProgress
                ? `Checking duplicates ${dedupProgress.done} / ${dedupProgress.total}…`
                : `${files.length} files · ${dupCount} duplicate`}
            </span>
          )}
        </div>

        <div style={{ display: "flex", flex: 1, minHeight: 0 }}>
          {/* Left: file list (or empty/loading state) */}
          <div
            style={{
              flex: 1,
              display: "flex",
              flexDirection: "column",
              minWidth: 0,
            }}
          >
            {!source ? (
              <EmptyState onChoose={chooseSource} />
            ) : listing ? (
              <Centered>Listing files…</Centered>
            ) : files.length === 0 ? (
              <Centered>No RAW files found in this folder.</Centered>
            ) : (
              <>
                <div style={toolbar()}>
                  <ToolbarBtn
                    onClick={() => setAll((f) => selectable(f, skipDuplicates))}
                  >
                    Select all
                  </ToolbarBtn>
                  <ToolbarBtn onClick={() => setSelected(new Set())}>
                    None
                  </ToolbarBtn>
                  <ToolbarBtn onClick={() => setAll((f) => f.status === "new")}>
                    Only new
                  </ToolbarBtn>
                  <span
                    style={{ marginLeft: "auto", color: "var(--color-t3)" }}
                  >
                    {selected.size} of {selectableCount} selected
                  </span>
                </div>
                <div style={{ flex: 1, overflowY: "auto" }}>
                  {groups.map((g) => (
                    <DayGroup
                      key={g.day}
                      group={g}
                      selected={selected}
                      skipDuplicates={skipDuplicates}
                      previewPath={previewPath}
                      onToggle={toggle}
                      onPreview={preview}
                      onToggleGroup={(paths, on) =>
                        setSelected((prev) => {
                          const next = new Set(prev);
                          for (const p of paths)
                            on ? next.add(p) : next.delete(p);
                          return next;
                        })
                      }
                    />
                  ))}
                </div>
              </>
            )}
          </div>

          {/* Right: preview + options */}
          <div style={rightPane()}>
            <div style={previewBox()}>
              {previewError ? (
                <span style={previewHint()}>No preview</span>
              ) : previewUrl ? (
                <img
                  src={previewUrl}
                  alt="preview"
                  style={{
                    maxWidth: "100%",
                    maxHeight: "100%",
                    objectFit: "contain",
                  }}
                />
              ) : previewPath ? (
                <span style={previewHint()}>Loading…</span>
              ) : (
                <span style={previewHint()}>Click a file to preview</span>
              )}
            </div>

            <div
              style={{
                overflowY: "auto",
                padding: "12px 14px",
                display: "flex",
                flexDirection: "column",
                gap: 14,
              }}
            >
              <Field label="Import method">
                <div
                  style={{ display: "flex", flexDirection: "column", gap: 6 }}
                >
                  {MODES.map((m) => (
                    <button
                      key={m.mode}
                      onClick={() => setMode(m.mode)}
                      style={modeBtn(mode === m.mode)}
                    >
                      <span style={{ fontWeight: 600, fontSize: 12.5 }}>
                        {m.label}
                      </span>
                      <span
                        style={{
                          fontSize: 11,
                          color: "var(--color-t3)",
                          lineHeight: 1.4,
                        }}
                      >
                        {m.desc}
                      </span>
                    </button>
                  ))}
                </div>
              </Field>

              {mode !== "reference" && (
                <Field label="Destination">
                  <div
                    style={{
                      fontSize: 11.5,
                      color: "var(--color-t2)",
                      wordBreak: "break-all",
                    }}
                  >
                    {dest ?? "Not set"}
                    <button
                      onClick={() =>
                        void pickFolder("Select library destination").then(
                          (p) => p && setDest(p),
                        )
                      }
                      style={{ ...linkBtn(), marginLeft: 6 }}
                    >
                      Change…
                    </button>
                    <div style={{ color: "var(--color-t3)", marginTop: 2 }}>
                      Filed under YYYY/YYYY-MM-DD by capture date.
                    </div>
                  </div>
                </Field>
              )}

              {mode === "move" && (
                <div style={{ fontSize: 11.5, color: "#c1916b" }}>
                  Originals are sent to Trash after a verified copy.
                </div>
              )}

              <label style={checkRow()}>
                <input
                  type="checkbox"
                  checked={recursive}
                  onChange={(e) => toggleRecursive(e.target.checked)}
                  style={{ accentColor: "var(--color-accent)" }}
                />
                Include subfolders
              </label>

              <label style={checkRow()}>
                <input
                  type="checkbox"
                  checked={skipDuplicates}
                  onChange={(e) => toggleSkipDuplicates(e.target.checked)}
                  style={{ accentColor: "var(--color-accent)" }}
                />
                Skip duplicates (hash-verified)
              </label>

              <Field label="Rating">
                <div style={{ display: "flex", gap: 4 }}>
                  {[1, 2, 3, 4, 5].map((n) => (
                    <button
                      key={n}
                      onClick={() => setRating(rating === n ? 0 : n)}
                      aria-label={`${n} star`}
                      style={{
                        ...iconToggle(),
                        color:
                          n <= rating
                            ? "var(--color-accent)"
                            : "var(--color-t3)",
                      }}
                    >
                      <Icon name="star" size={15} />
                    </button>
                  ))}
                </div>
              </Field>

              <Field label="Flag">
                <div style={{ display: "flex", gap: 6 }}>
                  {(["none", "pick", "reject"] as const).map((f) => (
                    <button
                      key={f}
                      onClick={() => setFlag(f)}
                      style={pillBtn(flag === f)}
                    >
                      {f === "none" ? "None" : f === "pick" ? "Pick" : "Reject"}
                    </button>
                  ))}
                </div>
              </Field>

              <Field label="Keywords">
                <input
                  value={keywordsText}
                  onChange={(e) => setKeywordsText(e.target.value)}
                  placeholder="comma, separated"
                  style={textInput()}
                />
              </Field>

              <Field label="Add to collection">
                <select
                  value={collChoice}
                  onChange={(e) => setCollChoice(e.target.value)}
                  style={textInput()}
                >
                  <option value="">None</option>
                  <option value="new">New collection…</option>
                  {collections
                    .filter((c) => !c.isSmart)
                    .map((c) => (
                      <option key={c.id} value={String(c.id)}>
                        {c.name}
                      </option>
                    ))}
                </select>
                {collChoice === "new" && (
                  <input
                    value={newCollName}
                    onChange={(e) => setNewCollName(e.target.value)}
                    placeholder="Collection name"
                    style={{ ...textInput(), marginTop: 6 }}
                  />
                )}
              </Field>
            </div>
          </div>
        </div>

        {/* Footer */}
        <div style={footer()}>
          <span style={{ marginLeft: "auto" }} />
          <button onClick={onClose} style={ghostBtn()}>
            Cancel
          </button>
          <button
            onClick={doImport}
            disabled={selected.size === 0}
            style={primaryBtn(selected.size === 0)}
          >
            Import {selected.size} selected
          </button>
        </div>
      </div>
    </div>
  );
}

function EmptyState({ onChoose }: { onChoose: () => void }) {
  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 12,
        color: "var(--color-t3)",
      }}
    >
      <Icon
        name="import"
        size={28}
        style={{ color: "var(--color-t3)" } as React.CSSProperties}
      />
      <div style={{ fontSize: 13 }}>
        Choose a card or folder to import from.
      </div>
      <button onClick={onChoose} style={primaryBtn(false)}>
        Select source folder
      </button>
    </div>
  );
}

function Centered({ children }: { children: React.ReactNode }) {
  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        color: "var(--color-t3)",
        fontSize: 13,
      }}
    >
      {children}
    </div>
  );
}

function DayGroup({
  group,
  selected,
  skipDuplicates,
  previewPath,
  onToggle,
  onPreview,
  onToggleGroup,
}: {
  group: { day: string; items: SourceFile[] };
  selected: Set<string>;
  skipDuplicates: boolean;
  previewPath: string | null;
  onToggle: (path: string) => void;
  onPreview: (path: string) => void;
  onToggleGroup: (paths: string[], on: boolean) => void;
}) {
  const selectablePaths = group.items
    .filter((f) => selectable(f, skipDuplicates))
    .map((f) => f.path);
  const allOn =
    selectablePaths.length > 0 && selectablePaths.every((p) => selected.has(p));

  return (
    <div>
      <div style={groupHead()}>
        <input
          type="checkbox"
          checked={allOn}
          disabled={selectablePaths.length === 0}
          onChange={(e) => onToggleGroup(selectablePaths, e.target.checked)}
          style={{ accentColor: "var(--color-accent)" }}
        />
        <span>{group.day}</span>
        <span style={{ color: "var(--color-t3)", fontWeight: 400 }}>
          {group.items.length}
        </span>
      </div>
      {group.items.map((f) => {
        const badge = DUP_BADGE[f.status];
        const disabled = !selectable(f, skipDuplicates);
        const active = previewPath === f.path;
        return (
          <div
            key={f.path}
            onClick={() => onPreview(f.path)}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 8,
              padding: "5px 12px 5px 22px",
              fontSize: 12,
              cursor: "pointer",
              color: "var(--color-t2)",
              background: active ? "var(--color-accent-dim)" : "transparent",
              opacity: disabled ? 0.5 : 1,
            }}
          >
            <input
              type="checkbox"
              checked={selected.has(f.path)}
              disabled={disabled}
              onChange={() => onToggle(f.path)}
              onClick={(e) => e.stopPropagation()}
              style={{ accentColor: "var(--color-accent)" }}
            />
            <span
              style={{
                flex: 1,
                overflow: "hidden",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {f.filename}
            </span>
            {badge && (
              <span style={{ fontSize: 9.5, color: "#b08968" }}>{badge}</span>
            )}
            <span
              style={{
                fontFamily: "var(--font-mono)",
                fontSize: 10.5,
                color: "var(--color-t3)",
              }}
            >
              {fmtSize(f.sizeBytes)}
            </span>
          </div>
        );
      })}
    </div>
  );
}

// ── styles ───────────────────────────────────────────────────────────────

const overlay = (): React.CSSProperties => ({
  position: "fixed",
  inset: 0,
  background: "rgba(0,0,0,.5)",
  backdropFilter: "blur(2px)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 50,
  padding: "4vh 4vw",
});

const panel = (): React.CSSProperties => ({
  width: 1040,
  maxWidth: "96vw",
  height: "86vh",
  background: "#1e1e22",
  border: "1px solid var(--color-line-2)",
  borderRadius: "var(--radius-lg)",
  boxShadow: "0 24px 80px rgba(0,0,0,.6)",
  display: "flex",
  flexDirection: "column",
  overflow: "hidden",
});

const header = (): React.CSSProperties => ({
  display: "flex",
  alignItems: "center",
  gap: 10,
  padding: "12px 16px",
  borderBottom: "1px solid var(--color-line)",
});

const pathLabel = (): React.CSSProperties => ({
  fontSize: 12,
  color: "var(--color-t3)",
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
  maxWidth: 520,
});

const rightPane = (): React.CSSProperties => ({
  width: 320,
  borderLeft: "1px solid var(--color-line)",
  display: "flex",
  flexDirection: "column",
  minHeight: 0,
});

const previewBox = (): React.CSSProperties => ({
  height: 240,
  flexShrink: 0,
  borderBottom: "1px solid var(--color-line)",
  background: "#111",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  padding: 8,
});

const previewHint = (): React.CSSProperties => ({
  color: "var(--color-t3)",
  fontSize: 12,
});

const toolbar = (): React.CSSProperties => ({
  display: "flex",
  alignItems: "center",
  gap: 6,
  padding: "8px 12px",
  borderBottom: "1px solid var(--color-line)",
  fontSize: 12,
});

const groupHead = (): React.CSSProperties => ({
  display: "flex",
  alignItems: "center",
  gap: 8,
  padding: "6px 12px 4px",
  fontSize: 11.5,
  color: "var(--color-t3)",
  fontWeight: 600,
  letterSpacing: ".03em",
  position: "sticky",
  top: 0,
  background: "#1e1e22",
});

const footer = (): React.CSSProperties => ({
  display: "flex",
  alignItems: "center",
  gap: 10,
  padding: "10px 16px",
  borderTop: "1px solid var(--color-line)",
});

function Field({
  label,
  children,
}: {
  label: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <div
        style={{
          fontSize: 10.5,
          letterSpacing: ".06em",
          textTransform: "uppercase",
          color: "var(--color-t3)",
          fontWeight: 600,
          marginBottom: 6,
        }}
      >
        {label}
      </div>
      {children}
    </div>
  );
}

function modeBtn(active: boolean): React.CSSProperties {
  return {
    display: "flex",
    flexDirection: "column",
    gap: 2,
    textAlign: "left",
    padding: "7px 9px",
    borderRadius: "var(--radius-sm)",
    border: `1px solid ${active ? "var(--color-accent)" : "var(--color-line)"}`,
    background: active ? "var(--color-accent-dim)" : "transparent",
    color: "var(--color-t1)",
    cursor: "pointer",
  };
}

function pillBtn(active: boolean): React.CSSProperties {
  return {
    flex: 1,
    padding: "5px 8px",
    fontSize: 11.5,
    borderRadius: "var(--radius-sm)",
    border: `1px solid ${active ? "var(--color-accent)" : "var(--color-line)"}`,
    background: active ? "var(--color-accent-dim)" : "transparent",
    color: active ? "var(--color-t1)" : "var(--color-t2)",
    cursor: "pointer",
  };
}

function textInput(): React.CSSProperties {
  return {
    width: "100%",
    background: "var(--color-panel)",
    border: "1px solid var(--color-line)",
    borderRadius: "var(--radius-sm)",
    color: "var(--color-t1)",
    fontSize: 12,
    padding: "6px 8px",
    outline: "none",
  };
}

function checkRow(): React.CSSProperties {
  return {
    display: "flex",
    alignItems: "center",
    gap: 8,
    fontSize: 12,
    color: "var(--color-t2)",
    cursor: "pointer",
    userSelect: "none",
  };
}

function iconToggle(): React.CSSProperties {
  return {
    display: "flex",
    border: "none",
    background: "transparent",
    padding: 1,
    cursor: "pointer",
  };
}

function ToolbarBtn({
  children,
  onClick,
}: {
  children: React.ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        fontSize: 11.5,
        padding: "3px 8px",
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--color-line)",
        background: "transparent",
        color: "var(--color-t2)",
        cursor: "pointer",
      }}
    >
      {children}
    </button>
  );
}

function linkBtn(): React.CSSProperties {
  return {
    fontSize: 11.5,
    border: "none",
    background: "transparent",
    color: "var(--color-accent)",
    cursor: "pointer",
    padding: 0,
  };
}

function ghostBtn(): React.CSSProperties {
  return {
    fontSize: 12.5,
    padding: "7px 14px",
    borderRadius: "var(--radius-sm)",
    border: "1px solid var(--color-line)",
    background: "transparent",
    color: "var(--color-t2)",
    cursor: "pointer",
  };
}

function primaryBtn(disabled: boolean): React.CSSProperties {
  return {
    fontSize: 12.5,
    fontWeight: 600,
    padding: "7px 16px",
    borderRadius: "var(--radius-sm)",
    border: "1px solid var(--color-accent)",
    background: disabled ? "var(--color-line)" : "var(--color-accent)",
    color: disabled ? "var(--color-t3)" : "#11110f",
    cursor: disabled ? "default" : "pointer",
  };
}
