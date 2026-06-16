import { useState } from "react";
import Icon from "../../components/Icon";
import { type CollectionRow } from "../../lib/ipc";

const LABEL_SWATCHES: { key: string; bg: string }[] = [
  { key: "red", bg: "var(--color-lab-red)" },
  { key: "yellow", bg: "var(--color-lab-yellow)" },
  { key: "green", bg: "var(--color-lab-green)" },
  { key: "blue", bg: "var(--color-lab-blue)" },
  { key: "purple", bg: "var(--color-lab-purple)" },
];

interface SelectionBarProps {
  count: number;
  collections: CollectionRow[];
  onRate: (stars: number) => void;
  onFlag: (flag: "pick" | "reject" | "none") => void;
  onLabel: (label: string | null) => void;
  onAddKeyword: (name: string) => void;
  onAddToCollection: (collectionId: number) => void;
  onExport: () => void;
  onClear: () => void;
}

function Divider() {
  return (
    <span
      style={{ width: 1, height: 18, background: "var(--color-line)" }}
    />
  );
}

function btn(): React.CSSProperties {
  return {
    display: "flex",
    alignItems: "center",
    gap: 5,
    padding: "4px 9px",
    borderRadius: 20,
    border: "1px solid var(--color-line)",
    background: "transparent",
    color: "var(--color-t2)",
    fontSize: 12,
    cursor: "pointer",
    whiteSpace: "nowrap",
  };
}

export default function SelectionBar({
  count,
  collections,
  onRate,
  onFlag,
  onLabel,
  onAddKeyword,
  onAddToCollection,
  onExport,
  onClear,
}: SelectionBarProps) {
  const [kw, setKw] = useState("");
  const staticCollections = collections.filter((c) => !c.isSmart);

  function commitKw() {
    const name = kw.trim();
    if (name) onAddKeyword(name);
    setKw("");
  }

  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 10,
        padding: "0 14px",
        height: 42,
        flexShrink: 0,
        background: "var(--color-accent-dim)",
        borderBottom: "1px solid var(--color-accent-line)",
        overflowX: "auto",
      }}
    >
      <span
        style={{
          fontSize: 12,
          fontWeight: 600,
          color: "var(--color-t1)",
          whiteSpace: "nowrap",
        }}
      >
        {count} selected
      </span>

      <Divider />

      {/* Batch rating */}
      <div style={{ display: "flex", gap: 1 }} title="Set rating on selection">
        {[1, 2, 3, 4, 5].map((n) => (
          <svg
            key={n}
            viewBox="0 0 16 16"
            width={14}
            height={14}
            fill="none"
            stroke="var(--color-star)"
            strokeWidth="1.2"
            style={{ cursor: "pointer", display: "block" }}
            onClick={() => onRate(n)}
          >
            <path d="M8 2.2l1.7 3.6 3.9.5-2.9 2.7.8 3.9L8 11l-3.5 1.9.8-3.9L2.4 6.3l3.9-.5z" />
          </svg>
        ))}
      </div>

      <Divider />

      {/* Batch flags */}
      <button style={btn()} onClick={() => onFlag("pick")}>
        <Icon name="flag" size={12} /> Pick
      </button>
      <button style={btn()} onClick={() => onFlag("reject")}>
        Reject
      </button>
      <button
        style={btn()}
        onClick={() => onFlag("none")}
        title="Clear flag on selection"
      >
        Unflag
      </button>

      <Divider />

      {/* Batch color labels */}
      <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
        {LABEL_SWATCHES.map(({ key, bg }) => (
          <span
            key={key}
            onClick={() => onLabel(key)}
            title={`Label selection ${key}`}
            style={{
              width: 12,
              height: 12,
              borderRadius: "50%",
              background: bg,
              boxShadow: "0 0 0 1.5px rgba(0,0,0,.35)",
              cursor: "pointer",
              display: "block",
            }}
          />
        ))}
        <span
          onClick={() => onLabel(null)}
          title="Clear label on selection"
          style={{
            width: 12,
            height: 12,
            borderRadius: "50%",
            border: "1px solid var(--color-t3)",
            cursor: "pointer",
            display: "block",
          }}
        />
      </div>

      <Divider />

      {/* Batch keyword */}
      <input
        value={kw}
        onChange={(e) => setKw(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commitKw();
          }
        }}
        onBlur={commitKw}
        placeholder="Add keyword…"
        style={{
          width: 120,
          background: "var(--color-panel)",
          border: "1px solid var(--color-line)",
          borderRadius: "var(--radius-sm)",
          color: "var(--color-t1)",
          fontSize: 12,
          padding: "4px 8px",
          outline: "none",
        }}
      />

      {/* Batch add to collection */}
      {staticCollections.length > 0 && (
        <select
          value=""
          onChange={(e) => {
            const id = Number(e.target.value);
            if (id) onAddToCollection(id);
          }}
          style={{
            background: "var(--color-panel)",
            border: "1px solid var(--color-line)",
            borderRadius: "var(--radius-sm)",
            color: "var(--color-t2)",
            fontSize: 12,
            padding: "4px 8px",
            cursor: "pointer",
          }}
        >
          <option value="">Add to collection…</option>
          {staticCollections.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
      )}

      <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
        <button style={btn()} onClick={onExport}>
          <Icon name="export" size={12} /> Export
        </button>
        <button style={btn()} onClick={onClear} title="Clear selection">
          Done
        </button>
      </div>
    </div>
  );
}
