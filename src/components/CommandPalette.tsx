import { useEffect, useRef, useState } from "react";
import { useAppStore } from "../store/app";
import { runExport } from "../lib/export";
import { fmtShortcut } from "../lib/platform";
import Icon, { IconName } from "./Icon";

interface PaletteRow {
  icon: IconName;
  label: string;
  shortcut: string;
  run?: () => void;
}

export default function CommandPalette() {
  const open = useAppStore((s) => s.paletteOpen);
  const setPaletteOpen = useAppStore((s) => s.setPaletteOpen);
  const setView = useAppStore((s) => s.setView);
  const view = useAppStore((s) => s.view);
  const selectedId = useAppStore((s) => s.selectedId);
  const onImport = useAppStore((s) => s.onImport);
  const onOpenSettings = useAppStore((s) => s.onOpenSettings);
  const onSavePreset = useAppStore((s) => s.onSavePreset);
  const onCopySettings = useAppStore((s) => s.onCopySettings);
  const onPasteSettings = useAppStore((s) => s.onPasteSettings);
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (open) {
      setQuery(""); // fresh search each time the palette opens
      setTimeout(() => inputRef.current?.focus(), 20);
    }
  }, [open]);

  if (!open) return null;

  const libraryRows: PaletteRow[] = [
    {
      icon: "import",
      label: "Import from SD card…",
      shortcut: "⌘I",
      run: () => onImport?.(),
    },
    {
      icon: "copy",
      label: "Find duplicates",
      shortcut: "⌘D",
      run: () => setView("dedup"),
    },
    {
      icon: "edit",
      label: "Open in Develop",
      shortcut: "D",
      run: () => setView("develop"),
    },
    {
      icon: "export",
      label: "Export selected…",
      shortcut: "⌘E",
      run: () => void runExport(selectedId),
    },
    {
      icon: "bolt",
      label: "Settings…",
      shortcut: "⌘,",
      run: () => onOpenSettings?.(),
    },
  ];

  const developRows: PaletteRow[] = [
    { icon: "split", label: "Toggle before / after", shortcut: "\\" },
    {
      icon: "star",
      label: "Save as preset…",
      shortcut: "⌘⇧N",
      run: () => onSavePreset?.(),
    },
    {
      icon: "copy",
      label: "Copy settings",
      shortcut: "⌘⇧C",
      run: () => onCopySettings?.(),
    },
    {
      icon: "copy",
      label: "Paste settings",
      shortcut: "⌘⇧V",
      run: () => onPasteSettings?.(),
    },
    {
      icon: "export",
      label: "Export this photo…",
      shortcut: "⌘E",
      run: () => void runExport(selectedId),
    },
    {
      icon: "grid",
      label: "Back to Library",
      shortcut: "G",
      run: () => setView("library"),
    },
  ];

  const q = query.trim().toLowerCase();
  const allRows = view === "library" ? libraryRows : developRows;
  const rows = q
    ? allRows.filter((r) => r.label.toLowerCase().includes(q))
    : allRows;

  return (
    <div
      onClick={(e) => {
        if (e.target === e.currentTarget) setPaletteOpen(false);
      }}
      style={{
        position: "fixed",
        inset: 0,
        background: "rgba(0,0,0,.45)",
        backdropFilter: "blur(2px)",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        paddingTop: "14vh",
        zIndex: 50,
      }}
    >
      <div
        style={{
          width: 560,
          maxWidth: "92vw",
          background: "#26262a",
          border: "1px solid var(--color-line-2)",
          borderRadius: "var(--radius-lg)",
          boxShadow: "0 24px 80px rgba(0,0,0,.6)",
          overflow: "hidden",
        }}
      >
        {/* Search row */}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 10,
            padding: "14px 16px",
            borderBottom: "1px solid var(--color-line)",
          }}
        >
          <Icon
            name="cmd"
            size={18}
            style={{ color: "var(--color-t3)" } as React.CSSProperties}
          />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Type a command…"
            style={{
              flex: 1,
              background: "none",
              border: "none",
              color: "var(--color-t1)",
              fontSize: 15,
              outline: "none",
              fontFamily: "var(--font-ui)",
            }}
          />
        </div>

        {/* Command rows */}
        {rows.map((row, i) => (
          <div
            key={row.label}
            onClick={() => {
              row.run?.();
              setPaletteOpen(false);
            }}
            style={{
              display: "flex",
              alignItems: "center",
              gap: 11,
              padding: "10px 16px",
              color: i === 0 ? "var(--color-t1)" : "var(--color-t2)",
              fontSize: 13,
              background: i === 0 ? "var(--color-accent-dim)" : "transparent",
              cursor: "pointer",
            }}
          >
            <Icon name={row.icon} />
            {row.label}
            <span
              style={{
                marginLeft: "auto",
                fontFamily: "var(--font-mono)",
                fontSize: 11,
                color: "var(--color-t3)",
              }}
            >
              {fmtShortcut(row.shortcut)}
            </span>
          </div>
        ))}
      </div>
    </div>
  );
}
