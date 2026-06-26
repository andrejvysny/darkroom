import { useEffect, useRef, useState } from "react";
import Icon from "../../components/Icon";
import type { PresetSummary } from "../../lib/ipc";
import type { PresetsApi } from "./usePresets";

interface Props {
  api: PresetsApi;
  onOpenCreate: () => void;
}

const iconBtn: React.CSSProperties = {
  background: "none",
  border: "none",
  color: "var(--color-t2)",
  cursor: "pointer",
  padding: 4,
  display: "flex",
  alignItems: "center",
};

/** Groups in panel order, built-ins first (presets already arrive builtin-first, group, sort, name). */
function groupPresets(presets: PresetSummary[]): [string, PresetSummary[]][] {
  const order: string[] = [];
  const map = new Map<string, PresetSummary[]>();
  for (const p of presets) {
    if (!map.has(p.groupName)) {
      map.set(p.groupName, []);
      order.push(p.groupName);
    }
    map.get(p.groupName)!.push(p);
  }
  return order.map((g) => [g, map.get(g)!]);
}

export default function PresetsPanel({ api, onOpenCreate }: Props) {
  const [menuFor, setMenuFor] = useState<number | null>(null);
  const [renamingId, setRenamingId] = useState<number | null>(null);
  const [draft, setDraft] = useState("");
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const rootRef = useRef<HTMLDivElement>(null);

  // Close the row menu on any outside click.
  useEffect(() => {
    if (menuFor === null) return;
    const onClick = () => setMenuFor(null);
    window.addEventListener("click", onClick);
    return () => window.removeEventListener("click", onClick);
  }, [menuFor]);

  const grouped = groupPresets(api.presets);

  const toggleGroup = (g: string) =>
    setCollapsed((s) => {
      const n = new Set(s);
      if (n.has(g)) n.delete(g);
      else n.add(g);
      return n;
    });

  const startRename = (p: PresetSummary) => {
    setMenuFor(null);
    setRenamingId(p.id);
    setDraft(p.name);
  };
  const commitRename = () => {
    if (renamingId !== null && draft.trim()) {
      void api.renamePreset(renamingId, draft.trim());
    }
    setRenamingId(null);
  };

  return (
    <div ref={rootRef} style={{ display: "flex", flexDirection: "column" }}>
      {/* Header: import + new */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 4,
          padding: "8px 10px 8px 14px",
          borderBottom: "1px solid var(--color-line)",
        }}
      >
        <span
          style={{
            flex: 1,
            fontSize: 11,
            color: "var(--color-t3)",
            fontWeight: 600,
          }}
        >
          PRESETS
        </span>
        <button
          title="Import preset file…"
          style={iconBtn}
          onClick={() => void api.importPreset()}
        >
          <Icon name="import" size={14} />
        </button>
        <button
          title="New preset from current edit"
          style={iconBtn}
          onClick={onOpenCreate}
        >
          <span style={{ fontSize: 18, lineHeight: "14px" }}>+</span>
        </button>
      </div>

      {/* Amount + copy/paste */}
      <div
        style={{
          padding: "10px 14px",
          borderBottom: "1px solid var(--color-line)",
          display: "flex",
          flexDirection: "column",
          gap: 9,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ fontSize: 11, color: "var(--color-t3)", width: 48 }}>
            Amount
          </span>
          <input
            type="range"
            min={0}
            max={100}
            value={api.amount}
            onChange={(e) => api.setPresetAmount(Number(e.target.value))}
            style={{ flex: 1 }}
          />
          <span
            style={{
              fontSize: 11,
              fontFamily: "var(--font-mono)",
              color: "var(--color-t2)",
              width: 30,
              textAlign: "right",
            }}
          >
            {api.amount}
          </span>
        </div>
        <div style={{ display: "flex", gap: 6 }}>
          <button
            onClick={() => api.copySettings()}
            style={{
              flex: 1,
              border: "1px solid var(--color-line-2)",
              background: "var(--color-elev)",
              color: "var(--color-t1)",
              borderRadius: "var(--radius-sm)",
              padding: "5px 0",
              fontSize: 11,
              cursor: "pointer",
            }}
          >
            Copy settings
          </button>
          <button
            onClick={() => void api.pasteSettings()}
            disabled={!api.canPaste}
            style={{
              flex: 1,
              border: "1px solid var(--color-line-2)",
              background: "var(--color-elev)",
              color: api.canPaste ? "var(--color-t1)" : "var(--color-t3)",
              borderRadius: "var(--radius-sm)",
              padding: "5px 0",
              fontSize: 11,
              cursor: api.canPaste ? "pointer" : "default",
              opacity: api.canPaste ? 1 : 0.55,
            }}
          >
            Paste
          </button>
        </div>
      </div>

      {/* Grouped list */}
      {grouped.length === 0 && (
        <div style={{ padding: 16, fontSize: 12, color: "var(--color-t3)" }}>
          No presets yet. Use + to save the current edit.
        </div>
      )}
      {grouped.map(([group, items]) => {
        const isCollapsed = collapsed.has(group);
        return (
          <div key={group}>
            <div
              onClick={() => toggleGroup(group)}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 6,
                padding: "8px 14px",
                cursor: "pointer",
                userSelect: "none",
                color: "var(--color-t2)",
              }}
            >
              <Icon
                name="chev"
                size={11}
                style={
                  {
                    transform: isCollapsed ? "rotate(-90deg)" : "none",
                    transition: "transform .15s",
                  } as React.CSSProperties
                }
              />
              <span style={{ fontSize: 11.5, fontWeight: 600 }}>{group}</span>
              <span style={{ fontSize: 10, color: "var(--color-t3)" }}>
                ({items.length})
              </span>
            </div>

            {!isCollapsed &&
              items.map((p) => (
                <div
                  key={p.id}
                  onMouseEnter={() => {
                    if (renamingId === null) void api.hoverStart(p.id);
                  }}
                  onMouseLeave={() => api.hoverEnd()}
                  style={{
                    position: "relative",
                    display: "flex",
                    alignItems: "center",
                    gap: 6,
                    padding: "6px 10px 6px 26px",
                    cursor: "pointer",
                    fontSize: 12.5,
                    color: "var(--color-t1)",
                  }}
                  className="preset-row"
                >
                  {renamingId === p.id ? (
                    <input
                      autoFocus
                      value={draft}
                      onChange={(e) => setDraft(e.target.value)}
                      onBlur={commitRename}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") commitRename();
                        if (e.key === "Escape") setRenamingId(null);
                      }}
                      style={{
                        flex: 1,
                        background: "var(--color-elev)",
                        border: "1px solid var(--color-accent)",
                        borderRadius: "var(--radius-sm)",
                        padding: "3px 6px",
                        fontSize: 12.5,
                        color: "var(--color-t1)",
                        outline: "none",
                      }}
                    />
                  ) : (
                    <span
                      style={{ flex: 1 }}
                      onClick={() => void api.applyPreset(p.id)}
                    >
                      {p.name}
                    </span>
                  )}

                  <button
                    title={p.isFavorite ? "Unfavorite" : "Favorite"}
                    onClick={(e) => {
                      e.stopPropagation();
                      void api.toggleFavorite(p);
                    }}
                    style={{
                      ...iconBtn,
                      color: p.isFavorite
                        ? "var(--color-accent)"
                        : "var(--color-t3)",
                    }}
                  >
                    <Icon
                      name="star"
                      size={13}
                      style={
                        {
                          fill: p.isFavorite ? "currentColor" : "none",
                        } as React.CSSProperties
                      }
                    />
                  </button>

                  <button
                    title="More…"
                    onClick={(e) => {
                      e.stopPropagation();
                      setMenuFor(menuFor === p.id ? null : p.id);
                    }}
                    style={{
                      ...iconBtn,
                      color: "var(--color-t3)",
                      fontSize: 15,
                    }}
                  >
                    ⋯
                  </button>

                  {menuFor === p.id && (
                    <div
                      onClick={(e) => e.stopPropagation()}
                      style={{
                        position: "absolute",
                        right: 8,
                        top: 30,
                        zIndex: 5,
                        minWidth: 130,
                        background: "#2c2c30",
                        border: "1px solid var(--color-line-2)",
                        borderRadius: "var(--radius-sm)",
                        boxShadow: "0 10px 30px rgba(0,0,0,.5)",
                        overflow: "hidden",
                      }}
                    >
                      <MenuItem
                        label="Apply"
                        onClick={() => {
                          setMenuFor(null);
                          void api.applyPreset(p.id);
                        }}
                      />
                      <MenuItem
                        label="Apply (replace all)"
                        onClick={() => {
                          setMenuFor(null);
                          void api.applyPreset(p.id, true);
                        }}
                      />
                      <MenuItem
                        label="Duplicate"
                        onClick={() => {
                          setMenuFor(null);
                          void api.duplicatePreset(p.id);
                        }}
                      />
                      <MenuItem
                        label="Export…"
                        onClick={() => {
                          setMenuFor(null);
                          void api.exportPreset(p);
                        }}
                      />
                      {!p.builtin && (
                        <>
                          <MenuItem
                            label="Rename"
                            onClick={() => startRename(p)}
                          />
                          <MenuItem
                            label="Delete"
                            danger
                            onClick={() => {
                              setMenuFor(null);
                              void api.removePreset(p.id);
                            }}
                          />
                        </>
                      )}
                    </div>
                  )}
                </div>
              ))}
          </div>
        );
      })}
    </div>
  );
}

function MenuItem({
  label,
  onClick,
  danger,
}: {
  label: string;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <div
      onClick={onClick}
      style={{
        padding: "7px 12px",
        fontSize: 12,
        color: danger ? "#d77" : "var(--color-t1)",
        cursor: "pointer",
      }}
      onMouseEnter={(e) =>
        (e.currentTarget.style.background = "var(--color-elev)")
      }
      onMouseLeave={(e) => (e.currentTarget.style.background = "transparent")}
    >
      {label}
    </div>
  );
}
