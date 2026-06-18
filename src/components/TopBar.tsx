import { useRef, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useAppStore } from "../store/app";
import { useDevelopStore } from "../store/develop";
import Icon from "./Icon";

// Interactive controls (and anything opted out via [data-no-drag]) must NOT start a window drag.
const NO_DRAG =
  "button, input, a, select, label, [role='button'], [data-no-drag]";

// `data-tauri-drag-region` only drags when the moused-down element itself carries the attribute, so
// clicks on the header's child containers (e.g. the flex spacer) don't move the window. A mousedown
// handler that calls the window API is robust regardless of which child was hit.
function onTitlebarMouseDown(e: React.MouseEvent) {
  if (e.button !== 0) return; // primary button only
  if ((e.target as HTMLElement).closest(NO_DRAG)) return;
  if (e.detail === 2) void getCurrentWindow().toggleMaximize();
  else void getCurrentWindow().startDragging();
}

export default function TopBar() {
  const view = useAppStore((s) => s.view);
  const setView = useAppStore((s) => s.setView);
  const setPaletteOpen = useAppStore((s) => s.setPaletteOpen);
  const onImport = useAppStore((s) => s.onImport);
  const onOpenDedup = useAppStore((s) => s.onOpenDedup);
  const onSearch = useAppStore((s) => s.onSearch);
  const onDevelopReset = useAppStore((s) => s.onDevelopReset);
  const currentFilename = useAppStore(
    (s) => s.libraryImages.find((i) => i.id === s.selectedId)?.filename ?? "—",
  );
  const showBefore = useDevelopStore((s) => s.showBefore);
  const setShowBefore = useDevelopStore((s) => s.setShowBefore);

  const [search, setSearch] = useState("");
  const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function onSearchInput(value: string) {
    setSearch(value);
    if (searchTimer.current !== null) clearTimeout(searchTimer.current);
    searchTimer.current = setTimeout(() => onSearch?.(value), 300);
  }

  return (
    <header
      onMouseDown={onTitlebarMouseDown}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        // Left pad clears the native macOS traffic lights (overlaid via titleBarStyle:"Overlay").
        paddingLeft: 82,
        paddingRight: 14,
        background: "var(--color-app)",
        borderBottom: "1px solid var(--color-line)",
        height: 46,
        flexShrink: 0,
      }}
    >
      {/* Segmented control */}
      <div
        style={{
          display: "flex",
          background: "var(--color-elev)",
          border: "1px solid var(--color-line)",
          borderRadius: "var(--radius-sm)",
          padding: 2,
        }}
      >
        <button
          data-testid="nav-library"
          onClick={() => setView("library")}
          style={{
            padding: "4px 13px",
            borderRadius: 4,
            fontSize: 12.5,
            fontWeight: 500,
            color: view === "library" ? "var(--color-t1)" : "var(--color-t2)",
            background:
              view === "library" ? "var(--color-hover)" : "transparent",
            boxShadow: view === "library" ? "0 1px 0 rgba(0,0,0,.25)" : "none",
          }}
        >
          Library
        </button>
        <button
          data-testid="nav-develop"
          onClick={() => setView("develop")}
          style={{
            padding: "4px 13px",
            borderRadius: 4,
            fontSize: 12.5,
            fontWeight: 500,
            color: view === "develop" ? "var(--color-t1)" : "var(--color-t2)",
            background:
              view === "develop" ? "var(--color-hover)" : "transparent",
            boxShadow: view === "develop" ? "0 1px 0 rgba(0,0,0,.25)" : "none",
          }}
        >
          Develop
        </button>
      </div>

      {/* Context-aware right side */}
      {view === "library" ? (
        <>
          <label
            style={{
              flex: 1,
              maxWidth: 420,
              display: "flex",
              alignItems: "center",
              gap: 8,
              background: "var(--color-panel)",
              border: "1px solid var(--color-line)",
              borderRadius: "var(--radius-sm)",
              padding: "6px 10px",
              color: "var(--color-t3)",
            }}
          >
            <Icon name="search" />
            <input
              data-testid="library-search"
              placeholder="Search filename, camera, keyword…"
              value={search}
              onChange={(e) => onSearchInput(e.target.value)}
              style={{
                flex: 1,
                background: "none",
                border: "none",
                color: "var(--color-t1)",
                fontSize: 12.5,
                outline: "none",
              }}
            />
          </label>
          <div style={{ flex: 1 }} />
          <button className="tbtn ghost" onClick={() => onImport?.()}>
            <Icon name="import" />
            Import
          </button>
          <button className="tbtn ghost" onClick={() => onOpenDedup?.()}>
            <Icon name="copy" />
            Find duplicates
          </button>
        </>
      ) : (
        <>
          <div
            style={{
              display: "flex",
              alignItems: "center",
              gap: 9,
              margin: "0 auto",
              minWidth: 0,
              whiteSpace: "nowrap",
            }}
          >
            <span
              style={{
                fontFamily: "var(--font-mono)",
                fontSize: 12.5,
                color: "var(--color-t1)",
              }}
            >
              {currentFilename}
            </span>
            <span
              style={{
                fontSize: 10,
                fontWeight: 600,
                letterSpacing: ".04em",
                color: "var(--color-accent)",
                border: "1px solid var(--color-accent-line)",
                borderRadius: 4,
                padding: "2px 6px",
              }}
            >
              RAW · scene-linear
            </span>
          </div>
          <button
            className="tbtn ghost"
            onClick={() => setShowBefore(!showBefore)}
            style={
              showBefore
                ? { color: "var(--color-t1)", background: "var(--color-hover)" }
                : undefined
            }
          >
            <Icon name="split" />
            Before / After
          </button>
          <button className="tbtn ghost" onClick={() => onDevelopReset?.()}>
            <Icon name="reset" />
            Reset
          </button>
        </>
      )}

      <button className="tbtn" onClick={() => setPaletteOpen(true)}>
        <Icon name="cmd" />
        <span
          style={{
            fontFamily: "var(--font-mono)",
            fontSize: 11,
            background: "var(--color-elev)",
            border: "1px solid var(--color-line)",
            borderRadius: 4,
            padding: "2px 6px",
            color: "var(--color-t2)",
          }}
        >
          ⌘K
        </span>
      </button>
    </header>
  );
}
