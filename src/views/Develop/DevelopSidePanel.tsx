import { useState } from "react";
import Icon from "../../components/Icon";

interface Props {
  presetsContent: React.ReactNode;
  historyContent: React.ReactNode;
}

/** Left rail beside the Stage with Presets | History tabs (Lightroom-classic layout). Collapsible. */
export default function DevelopSidePanel({
  presetsContent,
  historyContent,
}: Props) {
  const [tab, setTab] = useState<"presets" | "history">("presets");
  const [collapsed, setCollapsed] = useState(false);

  if (collapsed) {
    return (
      <div
        style={{
          flexShrink: 0,
          width: 28,
          background: "var(--color-app)",
          borderRight: "1px solid var(--color-line)",
          display: "flex",
          justifyContent: "center",
          paddingTop: 10,
        }}
      >
        <button
          title="Show presets & history"
          onClick={() => setCollapsed(false)}
          style={{
            background: "none",
            border: "none",
            color: "var(--color-t3)",
            cursor: "pointer",
            padding: 4,
          }}
        >
          <Icon
            name="chev"
            size={14}
            style={{ transform: "rotate(-90deg)" } as React.CSSProperties}
          />
        </button>
      </div>
    );
  }

  const tabStyle = (active: boolean): React.CSSProperties => ({
    flex: 1,
    padding: "9px 4px",
    fontSize: 12,
    fontWeight: 600,
    textAlign: "center",
    cursor: "pointer",
    color: active ? "var(--color-t1)" : "var(--color-t3)",
    borderBottom: active
      ? "2px solid var(--color-accent)"
      : "2px solid transparent",
    userSelect: "none",
  });

  return (
    <aside
      style={{
        flexShrink: 0,
        width: 236,
        background: "var(--color-app)",
        borderRight: "1px solid var(--color-line)",
        display: "flex",
        flexDirection: "column",
        minHeight: 0,
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "stretch",
          borderBottom: "1px solid var(--color-line)",
        }}
      >
        <div
          style={tabStyle(tab === "presets")}
          onClick={() => setTab("presets")}
        >
          Presets
        </div>
        <div
          style={tabStyle(tab === "history")}
          onClick={() => setTab("history")}
        >
          History
        </div>
        <button
          title="Hide panel"
          onClick={() => setCollapsed(true)}
          style={{
            background: "none",
            border: "none",
            color: "var(--color-t3)",
            cursor: "pointer",
            padding: "0 8px",
          }}
        >
          <Icon
            name="chev"
            size={13}
            style={{ transform: "rotate(90deg)" } as React.CSSProperties}
          />
        </button>
      </div>
      <div
        style={{
          flex: 1,
          overflowY: "auto",
          overflowX: "hidden",
          minHeight: 0,
        }}
      >
        {tab === "presets" ? presetsContent : historyContent}
      </div>
    </aside>
  );
}
