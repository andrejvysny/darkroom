import { useRef, useState } from "react";
import { useAppStore } from "../store/app";
import { useDevelopStore } from "../store/develop";
import Icon from "./Icon";

export default function TopBar() {
  const view = useAppStore((s) => s.view);
  const setView = useAppStore((s) => s.setView);
  const setPaletteOpen = useAppStore((s) => s.setPaletteOpen);
  const onImport = useAppStore((s) => s.onImport);
  const onOpenDedup = useAppStore((s) => s.onOpenDedup);
  const onSearch = useAppStore((s) => s.onSearch);
  const onDevelopReset = useAppStore((s) => s.onDevelopReset);
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
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        padding: "0 14px",
        background: "var(--color-app)",
        borderBottom: "1px solid var(--color-line)",
        height: 46,
        flexShrink: 0,
      }}
    >
      {/* Traffic lights */}
      <div style={{ display: "flex", gap: 8, marginRight: 4 }}>
        <i
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            display: "block",
            background: "#e0655b",
          }}
        />
        <i
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            display: "block",
            background: "#dba84a",
          }}
        />
        <i
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            display: "block",
            background: "#62b95a",
          }}
        />
      </div>

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
              DSC00487.ARW
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
